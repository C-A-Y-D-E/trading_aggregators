use super::{SOLANA_CHAIN_ID, currencies};
use crate::aggregators::relay::{AppFeeRequest, Fees, decode_error};
use crate::error::Result;
use crate::solana::provider_fee::ProviderFee;
use crate::{Quote, Side, Trade, TradeError};

pub(super) fn request(trade: &Trade, fee: ProviderFee) -> Result<AppFeeRequest<String>> {
    // Relay supports only input-currency app fees on Solana; sells must charge SOL/USDC output.
    if trade.side != Side::Buy {
        return Err(TradeError::Build(
            "Relay API fees support buys only; use SDK fee collection for SOL/USDC sell fees"
                .into(),
        ));
    }
    let recipient = fee.fee.relay_fee_recipient().ok_or_else(|| {
        TradeError::Build(
            "Relay provider fees require with_relay_fee_recipient(EVM address)".into(),
        )
    })?;
    Ok(AppFeeRequest {
        recipient: recipient.to_owned(),
        fee: fee.fee.basis_points().to_string(),
    })
}

pub(super) fn apply(
    fees: &Fees<String>,
    trade: &Trade,
    fee: Option<ProviderFee>,
    quote: &mut Quote,
) -> Result<()> {
    let amount = fees
        .app
        .as_ref()
        .map(|app| app.amount.parse::<u64>())
        .transpose()
        .map_err(decode_error)?
        .unwrap_or(0);
    match fee {
        Some(fee) => {
            let app = fees
                .app
                .as_ref()
                .ok_or_else(|| decode_error("missing requested app fee"))?;
            if app.currency.chain_id != SOLANA_CHAIN_ID
                || app.currency.address != currencies(trade).0
            {
                return Err(decode_error(
                    "app fee is not charged in the requested SOL/USDC input",
                ));
            }
            fee.validate_amount(quote, amount, u64::from(fee.fee.basis_points()))?;
            quote.application_fee = amount;
        }
        None if amount != 0 => return Err(decode_error("unexpected app fee")),
        None => {}
    }
    Ok(())
}
