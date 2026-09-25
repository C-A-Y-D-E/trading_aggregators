use async_trait::async_trait;
use solana_instruction::Instruction;

use super::{route_mints, validated_quote};
use crate::aggregators::bloxroute::{
    BloxrouteApi, InstructionsRequest, InstructionsResponse, TransactionConfig, VENUE,
};
use crate::error::Result;
use crate::solana::dexes::common::{
    COMPUTE_BUDGET_PROGRAM, REQUEST_HEAP_FRAME, SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT,
    set_compute_unit_limit,
};
use crate::solana::provider_fee::ProviderFee;
use crate::{Dex, PreparedSwap, Pubkey, Quote, Trade, TradeError, UsdValue};

const PROGRAM: Pubkey = Pubkey::from_str_const("BLXJD1miMgFTjCNR9B9KRPnhXMQZQaANvC7EGRVeTphD");

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
            })
            .await?;
        response.prepare_with_fee(trade, fee)
    }
}

#[async_trait]
impl Dex for Bloxroute {
    fn name(&self) -> &'static str {
        VENUE
    }

    async fn quote(&self, trade: &Trade) -> anyhow::Result<Quote> {
        Ok(self.prepare_with_fee(trade, None).await?.quote)
    }

    async fn prepare_swap(&self, trade: &Trade) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_fee(trade, None).await?)
    }

    async fn prepare_sponsored_swap(
        &self,
        _trade: &Trade,
        _sponsor: &Pubkey,
    ) -> anyhow::Result<PreparedSwap> {
        // The swap instruction creates missing token accounts itself, with the user as its only
        // signer and rent payer, so a sponsor can't take over that rent from outside.
        // The API's `payer` parameter is also the asset owner.
        anyhow::bail!(
            "bloXroute routes charge token-account rent to the user inside the swap; sponsored swaps are unsupported"
        )
    }
}

impl InstructionsResponse {
    fn prepare_with_fee(&self, trade: &Trade, fee: Option<ProviderFee>) -> Result<PreparedSwap> {
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
        let mut instructions = self.transaction_config.instructions()?;
        if let Some(fee) = fee {
            instructions.push(fee.setup(&trade.wallet));
        }
        for instruction in &self.setup_instructions {
            let instruction = instruction.decode_base64(VENUE, trade.wallet, None)?;
            if instruction.program_id == COMPUTE_BUDGET_PROGRAM {
                return Err(decode_error(
                    "unexpected compute budget in setup instructions",
                ));
            }
            instructions.push(instruction);
        }
        let swap = self
            .swap_instruction
            .decode_base64(VENUE, trade.wallet, None)?;
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
        })
    }
}

impl TransactionConfig {
    fn instructions(&self) -> Result<Vec<Instruction>> {
        if !(1..=1_400_000).contains(&self.compute_unit_limit)
            || !(1..=67_108_864).contains(&self.loaded_accounts_data_size_limit)
            || self.priority_fee.is_some_and(|fee| fee != 0)
        {
            return Err(decode_error("unsupported transaction configuration"));
        }
        // Translate v1 configuration to v0 budgets; the executor preserves the requested CU floor.
        // v0 still needs caller-supplied ALTs for large routes and enforces the 1232-byte wire limit.
        let mut instructions = vec![
            set_compute_unit_limit(self.compute_unit_limit),
            budget(
                SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT,
                self.loaded_accounts_data_size_limit,
            ),
        ];
        if let Some(heap) = self.heap_size {
            if !(32_768..=262_144).contains(&heap) || !heap.is_multiple_of(1024) {
                return Err(decode_error("invalid heap size"));
            }
            instructions.push(budget(REQUEST_HEAP_FRAME, heap));
        }
        Ok(instructions)
    }
}

fn budget(kind: u8, value: u32) -> Instruction {
    let mut data = vec![kind];
    data.extend_from_slice(&value.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM,
        accounts: vec![],
        data,
    }
}

fn decode_error(error: impl std::fmt::Display) -> TradeError {
    TradeError::Decode(VENUE, error.to_string())
}
