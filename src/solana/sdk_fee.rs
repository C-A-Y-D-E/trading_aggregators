use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::USDC_MINT;
use crate::error::{Result, TradeError};
use crate::solana::dexes::common::{TOKEN_PROGRAM, ata, create_ata_idempotent, system_transfer};
use crate::solana::types::{BASIS_POINTS, PreparedSwap, Quote, Settlement, Side, Trade};

const USDC_DECIMALS: u8 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeeCollection {
    #[default]
    Sdk,
    /// bloXroute/DFlow input/output fees, or Relay input fees on buys with an EVM claim address.
    Provider,
}

/// Optional, bypassable SDK fee in the trade's settlement currency; no custom program required.
#[derive(Clone, Copy, Debug)]
pub struct SdkFee {
    recipient: Pubkey,
    basis_points: u16,
    collection: FeeCollection,
    relay_fee_recipient: Option<[u8; 42]>,
}

impl SdkFee {
    /// Zero basis points disables charging.
    pub fn new(recipient: Pubkey, basis_points: u16) -> Result<Self> {
        if recipient == Pubkey::default() || u64::from(basis_points) >= BASIS_POINTS {
            return Err(TradeError::Build(
                "SDK fee requires a nonzero recipient and fee below 10000 bps".into(),
            ));
        }
        Ok(Self {
            recipient,
            basis_points,
            collection: FeeCollection::Sdk,
            relay_fee_recipient: None,
        })
    }

    pub fn recipient(&self) -> Pubkey {
        self.recipient
    }

    pub fn basis_points(&self) -> u16 {
        self.basis_points
    }

    pub fn with_collection(mut self, collection: FeeCollection) -> Self {
        self.collection = collection;
        self
    }

    pub fn collection(&self) -> FeeCollection {
        self.collection
    }

    /// Sets Relay's EVM app-fee claim address. Requires `FeeCollection::Provider`.
    /// Relay accrues USDC for later withdrawal; the Solana recipient remains used by other sources.
    pub fn with_relay_fee_recipient(mut self, recipient: &str) -> Result<Self> {
        let valid = recipient.strip_prefix("0x").is_some_and(|address| {
            address.len() == 40
                && address.bytes().all(|byte| byte.is_ascii_hexdigit())
                && address.bytes().any(|byte| byte != b'0')
        });
        if !valid {
            return Err(TradeError::Build(
                "Relay app fee recipient must be a nonzero EVM address (0x and 40 hex digits)"
                    .into(),
            ));
        }
        self.relay_fee_recipient = Some(recipient.as_bytes().try_into().expect("validated length"));
        Ok(self)
    }

    pub fn relay_fee_recipient(&self) -> Option<&str> {
        self.relay_fee_recipient
            .as_ref()
            .map(|address| std::str::from_utf8(address).expect("validated ASCII address"))
    }

    fn amount(&self, basis: u64) -> u64 {
        (u128::from(basis) * u128::from(self.basis_points) / u128::from(BASIS_POINTS)) as u64
    }

    pub(crate) fn trade_after_fee(&self, trade: &Trade) -> Result<Trade> {
        trade.validate()?;
        if self.basis_points > 0 && trade.wallet == self.recipient {
            return Err(TradeError::Build(
                "SDK fee recipient must differ from trading wallet".into(),
            ));
        }
        let mut adjusted = *trade;
        if trade.side == Side::Buy {
            adjusted.amount -= self.amount(trade.amount);
        }
        Ok(adjusted)
    }

    pub(crate) fn quote_after_fee(&self, trade: &Trade, mut quote: Quote) -> Result<Quote> {
        if quote.min_out == 0 || quote.expected_out < quote.min_out {
            return Err(TradeError::Build(
                "SDK fee route requires a positive, valid minimum output".into(),
            ));
        }
        quote.in_amount = trade.amount;
        // Sell fees are fixed from this quote's expected output, not recomputed from actual proceeds.
        quote.application_fee = self.amount(match trade.side {
            Side::Buy => trade.amount,
            Side::Sell => quote.expected_out,
        });
        if trade.side == Side::Sell {
            if quote.application_fee >= quote.min_out {
                return Err(TradeError::Build(
                    "minimum output must exceed the sell fee calculated from expected output"
                        .into(),
                ));
            }
            quote.expected_out -= quote.application_fee;
            quote.min_out -= quote.application_fee;
        }
        Ok(quote)
    }

    pub(crate) fn apply_with_payer(
        &self,
        trade: &Trade,
        mut prepared: PreparedSwap,
        payer: &Pubkey,
    ) -> Result<PreparedSwap> {
        prepared.quote = self.quote_after_fee(trade, prepared.quote)?;
        if prepared.quote.application_fee > 0 {
            // Append after the full route, never per hop; a failed transfer rolls back the swap.
            match trade.settlement {
                Settlement::Sol => prepared.instructions.push(system_transfer(
                    &trade.wallet,
                    &self.recipient,
                    prepared.quote.application_fee,
                )),
                Settlement::Usdc => {
                    append_usdc_fee(
                        &mut prepared.instructions,
                        payer,
                        &trade.wallet,
                        &self.recipient,
                        prepared.quote.application_fee,
                    )?;
                }
            }
        }
        Ok(prepared)
    }
}

pub(crate) fn append_usdc_fee(
    instructions: &mut Vec<Instruction>,
    payer: &Pubkey,
    wallet: &Pubkey,
    recipient: &Pubkey,
    amount: u64,
) -> Result<()> {
    instructions.push(create_ata_idempotent(
        payer,
        recipient,
        &USDC_MINT,
        &TOKEN_PROGRAM,
    ));
    instructions.push(
        spl_token::instruction::transfer_checked(
            &TOKEN_PROGRAM,
            &ata(wallet, &USDC_MINT, &TOKEN_PROGRAM),
            &USDC_MINT,
            &ata(recipient, &USDC_MINT, &TOKEN_PROGRAM),
            wallet,
            &[],
            amount,
            USDC_DECIMALS,
        )
        .map_err(|error| TradeError::Build(format!("USDC fee transfer: {error}")))?,
    );
    Ok(())
}
