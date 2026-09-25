use std::time::Instant;

use serde::Serialize;

use crate::Result;
use crate::aggregators::relay::{
    AppFeeRequest, Fees, QuoteRequest, QuoteResponse, RelayApi, Swap, VENUE, decode_error,
};
use crate::evm::{
    Address, Amount, AppFee, Currency, FeeAmount, Network, PreparedSwap, Quote, Trade,
};

mod transactions;

pub(crate) struct Relay {
    api: RelayApi,
}

impl Relay {
    pub fn new() -> Self {
        Self {
            api: RelayApi::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.api = self.api.with_base_url(base_url);
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api = self.api.with_api_key(api_key);
        self
    }

    pub async fn prepare<C: Network>(
        &self,
        trade: &Trade,
        fee: Option<AppFee>,
    ) -> Result<PreparedSwap> {
        trade.validate()?;
        let swap = Swap {
            wallet: trade.wallet,
            chain_id: C::CHAIN_ID,
            input: trade.input.address(),
            output: trade.output.address(),
            amount: trade.amount.to_string(),
            slippage_bps: trade.slippage_bps.to_string(),
        };
        let app_fees = fee
            .map(|fee| AppFeeRequest {
                recipient: fee.recipient(),
                fee: fee.basis_points().to_string(),
            })
            .into_iter()
            .collect();
        let options = EvmOptions {
            use_permit: false,
            subsidize_fees: false,
        };
        let created_at = Instant::now();
        let response: QuoteResponse<Address, transactions::ApiTransaction> = self
            .api
            .quote(&QuoteRequest::new(&swap, options, app_fees))
            .await?;
        let quote = quote::<C>(&response, &swap, trade, fee)?;
        let transactions = transactions::decode::<C>(response.steps, trade)?;
        Ok(PreparedSwap {
            quote,
            transactions,
            wallet: trade.wallet,
            created_at,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvmOptions {
    use_permit: bool,
    subsidize_fees: bool,
}

fn quote<C: Network>(
    response: &QuoteResponse<Address, transactions::ApiTransaction>,
    swap: &Swap<Address>,
    trade: &Trade,
    fee: Option<AppFee>,
) -> Result<Quote> {
    let details = &response.details;
    if !details.matches(swap) {
        return Err(decode_error(
            "quote does not match the requested chain, wallet or currencies",
        ));
    }
    let input = decimal(&details.currency_in.amount)?;
    let output = decimal(&details.currency_out.amount)?;
    let minimum = decimal(&details.currency_out.minimum_amount)?;
    if input != trade.amount
        || minimum.is_zero()
        || minimum > output
        || minimum < portion(output, 10_000 - trade.slippage_bps)
    {
        return Err(decode_error("invalid quote amounts or slippage"));
    }
    Ok(Quote {
        chain_id: C::CHAIN_ID,
        source: VENUE,
        input: trade.input,
        output: trade.output,
        in_amount: input,
        expected_out: output,
        min_out: minimum,
        application_fee: app_fee::<C>(&response.fees, trade, fee)?,
        usd_value: details.usd_value(),
    })
}

fn app_fee<C: Network>(
    fees: &Fees<Address>,
    trade: &Trade,
    requested: Option<AppFee>,
) -> Result<Option<FeeAmount>> {
    let Some(app) = &fees.app else {
        return if requested.is_some() {
            Err(decode_error("missing requested app fee"))
        } else {
            Ok(None)
        };
    };
    let amount = decimal(&app.amount)?;
    let Some(requested) = requested else {
        return if amount.is_zero() {
            Ok(None)
        } else {
            Err(decode_error("unexpected app fee"))
        };
    };
    if app.currency.chain_id != C::CHAIN_ID {
        return Err(decode_error("app fee is on a different chain"));
    }
    // Relay may collect another currency via a conversion; preserve its units instead of treating them as input units.
    if app.currency.address == trade.input.address()
        && amount != portion(trade.amount, requested.basis_points())
    {
        return Err(decode_error("app fee differs from the requested rate"));
    }
    let currency = if app.currency.address.is_zero() {
        Currency::Native
    } else {
        Currency::Token(app.currency.address)
    };
    Ok(Some(FeeAmount { currency, amount }))
}

fn decimal(value: &str) -> Result<Amount> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(decode_error("expected a decimal integer amount"));
    }
    Amount::from_str_radix(value, 10).map_err(decode_error)
}

fn portion(amount: Amount, bps: u16) -> Amount {
    let denominator = Amount::from(10_000);
    let numerator = Amount::from(bps);
    (amount / denominator) * numerator + (amount % denominator) * numerator / denominator
}
