use solana_instruction::Instruction;

use crate::error::Result;
use crate::solana::dexes::common::{TOKEN_PROGRAM, ata, create_ata_idempotent};
use crate::solana::types::BASIS_POINTS;
use crate::{Pubkey, Quote, SdkFee, Side, Trade, TradeError};

#[derive(Clone, Copy)]
pub(crate) struct ProviderFee {
    pub fee: SdkFee,
    pub mint: Pubkey,
    side: Side,
}

impl ProviderFee {
    pub fn new(fee: SdkFee, trade: &Trade) -> Result<Self> {
        fee.trade_after_fee(trade)?;
        Ok(Self {
            fee,
            side: trade.side,
            mint: trade.settlement.mint(),
        })
    }

    pub fn account(&self) -> Pubkey {
        ata(&self.fee.recipient(), &self.mint, &TOKEN_PROGRAM)
    }

    pub fn mode(&self) -> &'static str {
        match self.side {
            Side::Buy => "inputMint",
            Side::Sell => "outputMint",
        }
    }

    pub fn setup(&self, payer: &Pubkey) -> Instruction {
        create_ata_idempotent(payer, &self.fee.recipient(), &self.mint, &TOKEN_PROGRAM)
    }

    pub fn validate_amount(&self, quote: &Quote, amount: u64, bps: u64) -> Result<()> {
        let basis = match self.side {
            Side::Buy => quote.in_amount,
            Side::Sell => quote
                .expected_out
                .checked_add(amount)
                .ok_or_else(|| TradeError::Build("provider fee amount overflow".into()))?,
        };
        let expected = (u128::from(basis) * u128::from(self.fee.basis_points())
            / u128::from(BASIS_POINTS)) as u64;
        if bps != u64::from(self.fee.basis_points()) || amount != expected {
            return Err(TradeError::Build(
                "provider fee does not match the requested settlement fee".into(),
            ));
        }
        Ok(())
    }
}
