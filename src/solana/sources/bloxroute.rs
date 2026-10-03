use async_trait::async_trait;

use super::{move_rent_to_sponsor, route_mints, validated_quote};
use crate::aggregators::bloxroute::{
    BloxrouteApi, InstructionsRequest, InstructionsResponse, VENUE,
};
use crate::error::Result;
use crate::solana::dexes::common::COMPUTE_BUDGET_PROGRAM;
use crate::solana::provider_fee::ProviderFee;
use crate::{Dex, PreparedSwap, Pubkey, Quote, Trade, TradeError, TransactionFormat, UsdValue};

const PROGRAM: Pubkey = Pubkey::from_str_const("BLXJD1miMgFTjCNR9B9KRPnhXMQZQaANvC7EGRVeTphD");
const MAX_V1_ACCOUNTS: u8 = 64;
/// The SDK may add a submitter tip account, a fee recipient and its USDC account after the
/// route. The programs those use already appear in bloXroute routes.
const SDK_EXTRA_ACCOUNTS: u8 = 3;
/// Sponsorship adds the reimbursement recipient and its USDC account; bloXroute already
/// counts the sponsor itself as `feePayer`.
const SPONSOR_EXTRA_ACCOUNTS: u8 = 2;

/// bloXroute's aggregator API; separate from `BloxrouteSubmitter`.
pub struct Bloxroute {
    api: BloxrouteApi,
}

impl Default for Bloxroute {
    fn default() -> Self {
        Self::new()
    }
}

impl Bloxroute {
    pub fn new() -> Self {
        Self {
            api: BloxrouteApi::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.api = self.api.with_base_url(base_url);
        self
    }

    pub fn with_auth_header(mut self, auth_header: impl Into<String>) -> Self {
        self.api = self.api.with_auth_header(auth_header);
        self
    }

    pub(crate) async fn prepare_with_fee(
        &self,
        trade: &Trade,
        payer: Option<&Pubkey>,
        fee: Option<ProviderFee>,
    ) -> Result<PreparedSwap> {
        trade.validate()?;
        let (input, output) = route_mints(trade);
        let response = self
            .api
            .swap_instructions(&InstructionsRequest {
                input_mint: input.to_string(),
                output_mint: output.to_string(),
                amount: trade.amount,
                payer: trade.wallet.to_string(),
                slippage_bps: trade.slippage_bps,
                wrap_unwrap_sol: true,
                priority_fee: 0,
                platform_fee_bps: fee.map(|fee| fee.fee.basis_points()),
                platform_fee_mode: fee.map(|fee| fee.mode()),
                platform_fee_account: fee.map(|fee| fee.account().to_string()),
                max_accounts: max_accounts(payer),
                fee_payer: payer.map(ToString::to_string),
            })
            .await?;
        response.prepare_with_fee(trade, payer, fee)
    }
}

#[async_trait]
impl Dex for Bloxroute {
    fn name(&self) -> &'static str {
        VENUE
    }

    async fn quote(&self, trade: &Trade) -> anyhow::Result<Quote> {
        Ok(self.prepare_with_fee(trade, None, None).await?.quote)
    }

    async fn prepare_swap(&self, trade: &Trade) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_fee(trade, None, None).await?)
    }

    async fn prepare_sponsored_swap(
        &self,
        trade: &Trade,
        sponsor: &Pubkey,
    ) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_fee(trade, Some(sponsor), None).await?)
    }
}

impl InstructionsResponse {
    fn prepare_with_fee(
        &self,
        trade: &Trade,
        payer: Option<&Pubkey>,
        fee: Option<ProviderFee>,
    ) -> Result<PreparedSwap> {
        let (input, output) = route_mints(trade);
        if self.input_mint != input.to_string() || self.output_mint != output.to_string() {
            return Err(decode_error(
                "response mints do not match the requested trade",
            ));
        }
        let mut quote = validated_quote(
            VENUE,
            trade,
            self.input_amount,
            self.output_amount,
            self.output_amount_min,
        )?;
        quote.usd_value = UsdValue::from_reported(self.input_value_usd, self.output_value_usd);
        match (fee, &self.platform_fee) {
            (Some(requested), Some(received)) => {
                if received.mint.as_deref() != Some(requested.mint.to_string().as_str())
                    || received.mode.as_deref() != Some(requested.mode())
                {
                    return Err(decode_error(
                        "provider fee mint or side does not match settlement",
                    ));
                }
                requested.validate_amount(&quote, received.amount, received.bps.into())?;
                quote.application_fee = received.amount;
            }
            (Some(_), None) => return Err(decode_error("missing requested platform fee")),
            (None, Some(received)) if received.amount != 0 || received.bps != 0 => {
                return Err(decode_error(
                    "unexpected platform fee; SDK fee is collected separately",
                ));
            }
            _ => {}
        }
        let mut instructions = self.transaction_config.instructions(VENUE)?;
        if let Some(fee) = fee {
            instructions.push(fee.setup(payer.unwrap_or(&trade.wallet)));
        }
        for instruction in &self.setup_instructions {
            let mut instruction = instruction.decode_base64(VENUE, trade.wallet, payer)?;
            if instruction.program_id == COMPUTE_BUDGET_PROGRAM {
                return Err(decode_error(
                    "unexpected compute budget in setup instructions",
                ));
            }
            if let Some(payer) = payer {
                move_rent_to_sponsor(VENUE, &mut instruction, trade.wallet, *payer)?;
            }
            instructions.push(instruction);
        }
        let swap = self
            .swap_instruction
            .decode_base64(VENUE, trade.wallet, payer)?;
        if swap.program_id != PROGRAM
            || swap.data.is_empty()
            || !swap
                .accounts
                .iter()
                .any(|account| account.pubkey == trade.wallet && account.is_signer)
        {
            return Err(decode_error("invalid bloXroute swap instruction or owner"));
        }
        instructions.push(swap);
        Ok(PreparedSwap {
            venue: VENUE,
            quote,
            instructions,
            lookup_tables: vec![],
            // bloXroute routes use ~60 inline accounts and return no lookup tables.
            format: TransactionFormat::V1,
        })
    }
}

fn max_accounts(payer: Option<&Pubkey>) -> u8 {
    let sponsor_extra = if payer.is_some() {
        SPONSOR_EXTRA_ACCOUNTS
    } else {
        0
    };
    MAX_V1_ACCOUNTS - SDK_EXTRA_ACCOUNTS - sponsor_extra
}

fn decode_error(error: impl std::fmt::Display) -> TradeError {
    TradeError::Decode(VENUE, error.to_string())
}
