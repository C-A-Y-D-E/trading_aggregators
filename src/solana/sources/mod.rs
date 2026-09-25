//! Aggregator adapters that turn provider responses into Solana instructions.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use solana_instruction::{AccountMeta, Instruction};

use crate::aggregators::ApiInstruction;
use crate::error::Result;
use crate::solana::dexes::common::{
    ATA_CREATE, ATA_CREATE_IDEMPOTENT, ATA_PROGRAM, COMPUTE_BUDGET_PROGRAM, REQUEST_HEAP_FRAME,
    SET_COMPUTE_UNIT_LIMIT, SET_COMPUTE_UNIT_PRICE, SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT,
    SYSTEM_PROGRAM, ata, ata_account, system_transfer,
};
use crate::solana::types::BASIS_POINTS;
use crate::{Pubkey, Quote, Side, Trade, TradeError};

pub mod bloxroute;
pub mod dflow;
pub mod jupiter;
pub mod relay;

const U32_BYTES: usize = size_of::<u32>();
const U64_BYTES: usize = size_of::<u64>();

pub(crate) fn route_mints(trade: &Trade) -> (Pubkey, Pubkey) {
    let settlement = trade.settlement.mint();
    match trade.side {
        Side::Buy => (settlement, trade.mint),
        Side::Sell => (trade.mint, settlement),
    }
}

pub(crate) fn validated_quote(
    venue: &'static str,
    trade: &Trade,
    input: u64,
    expected: u64,
    minimum: u64,
) -> Result<Quote> {
    trade.validate()?;
    let floor = u128::from(expected) * u128::from(BASIS_POINTS - trade.slippage_bps)
        / u128::from(BASIS_POINTS);
    if input != trade.amount || minimum == 0 || minimum > expected || u128::from(minimum) < floor {
        return Err(TradeError::Decode(
            venue,
            "invalid input, minimum output or slippage".into(),
        ));
    }
    Ok(Quote {
        in_amount: input,
        expected_out: expected,
        min_out: minimum,
        usd_value: None,
        fee: 0,
        application_fee: 0,
        sponsorship_fee: 0,
    })
}

impl ApiInstruction {
    pub fn decode_base64(
        &self,
        venue: &'static str,
        wallet: Pubkey,
        payer: Option<&Pubkey>,
    ) -> Result<Instruction> {
        let data = STANDARD
            .decode(&self.data)
            .map_err(|error| TradeError::Decode(venue, error.to_string()))?;
        self.decode(venue, wallet, payer, data)
    }

    pub fn decode_hex(
        &self,
        venue: &'static str,
        wallet: Pubkey,
        payer: Option<&Pubkey>,
    ) -> Result<Instruction> {
        if !self.data.len().is_multiple_of(2)
            || !self.data.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TradeError::Decode(
                venue,
                "invalid hex instruction data".into(),
            ));
        }
        let data = (0..self.data.len())
            .step_by(2)
            .map(|index| {
                u8::from_str_radix(&self.data[index..index + 2], 16).expect("validated hex")
            })
            .collect();
        self.decode(venue, wallet, payer, data)
    }

    fn decode(
        &self,
        venue: &'static str,
        wallet: Pubkey,
        payer: Option<&Pubkey>,
        data: Vec<u8>,
    ) -> Result<Instruction> {
        let invalid = |error: String| TradeError::Decode(venue, error);
        let accounts = self
            .accounts
            .iter()
            .map(|account| {
                let pubkey = account
                    .pubkey
                    .parse::<Pubkey>()
                    .map_err(|error| invalid(error.to_string()))?;
                if account.is_signer && pubkey != wallet && Some(&pubkey) != payer {
                    return Err(invalid("route requires an additional signer".into()));
                }
                Ok(AccountMeta {
                    pubkey,
                    is_signer: account.is_signer,
                    is_writable: account.is_writable,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Instruction {
            program_id: self
                .program_id
                .parse::<Pubkey>()
                .map_err(|error| invalid(error.to_string()))?,
            accounts,
            data,
        })
    }
}

/// The executor sets CU limit and price itself; heap and loaded-account requests are kept.
pub(crate) fn retain_compute_budget(
    venue: &'static str,
    instruction: &Instruction,
) -> Result<bool> {
    if instruction.program_id != COMPUTE_BUDGET_PROGRAM {
        return Ok(true);
    }
    let unsupported = || TradeError::Decode(venue, "unsupported compute-budget instruction".into());
    let [tag, value @ ..] = instruction.data.as_slice() else {
        return Err(unsupported());
    };
    match (*tag, value.len()) {
        (SET_COMPUTE_UNIT_LIMIT, U32_BYTES) | (SET_COMPUTE_UNIT_PRICE, U64_BYTES) => Ok(false),
        (REQUEST_HEAP_FRAME | SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT, U32_BYTES) => Ok(true),
        _ => Err(unsupported()),
    }
}

/// Rent paid inside the swap program itself can't be seen here; the executor's simulation
/// still fails such a route before anything is sent.
pub(crate) fn move_rent_to_sponsor(
    venue: &'static str,
    instruction: &mut Instruction,
    wallet: Pubkey,
    sponsor: Pubkey,
) -> Result<()> {
    let unsupported = |message: &str| TradeError::Decode(venue, message.into());
    if instruction.program_id == ATA_PROGRAM {
        let accounts = &mut instruction.accounts;
        if !matches!(
            instruction.data.as_slice(),
            [] | [ATA_CREATE] | [ATA_CREATE_IDEMPOTENT]
        ) || accounts.len() != ata_account::COUNT
            || ![wallet, sponsor].contains(&accounts[ata_account::PAYER].pubkey)
            || accounts[ata_account::SYSTEM_PROGRAM].pubkey != SYSTEM_PROGRAM
            || accounts[ata_account::ADDRESS].pubkey
                != ata(
                    &accounts[ata_account::OWNER].pubkey,
                    &accounts[ata_account::MINT].pubkey,
                    &accounts[ata_account::TOKEN_PROGRAM].pubkey,
                )
        {
            return Err(unsupported("unsupported sponsored token-account setup"));
        }
        accounts[ata_account::PAYER] = AccountMeta::new(sponsor, true);
    }
    let spends_user_sol = instruction.program_id == SYSTEM_PROGRAM
        && instruction
            .accounts
            .first()
            .is_some_and(|account| account.pubkey == wallet)
        && instruction.data != system_transfer(&wallet, &sponsor, 0).data;
    if spends_user_sol {
        return Err(unsupported("sponsored route still requires user SOL"));
    }
    Ok(())
}
