use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use solana_instruction::Instruction;

use super::{retain_compute_budget, route_mints, validated_quote};
use crate::aggregators::dflow::{
    DFlowApi, InstructionsRequest, InstructionsResponse, QuoteRequest, QuoteResponse,
    TRANSACTION_VERSION, VENUE, decode_error,
};
use crate::error::Result;
use crate::solana::lookup_table::load_address_lookup_tables;
use crate::solana::provider_fee::ProviderFee;
use crate::{Dex, PreparedSwap, Pubkey, Quote, RpcClient, Trade, TransactionFormat};

/// Atomic imperative swaps, including user-executed sponsorship; no intent/async orders.
pub struct DFlow {
    rpc: Arc<RpcClient>,
    api: DFlowApi,
}

impl DFlow {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            api: DFlowApi::new(),
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

    /// Returns the raw quote too, because `/swap-instructions` expects it back unchanged.
    async fn fetch_quote(
        &self,
        trade: &Trade,
        payer: Option<&Pubkey>,
        fee: Option<ProviderFee>,
    ) -> Result<(Value, Quote)> {
        trade.validate()?;
        let (input, output) = route_mints(trade);
        let sponsored = payer.is_some();
        let (raw, response) = self
            .api
            .quote(&QuoteRequest {
                input_mint: input.to_string(),
                output_mint: output.to_string(),
                amount: trade.amount,
                slippage_bps: trade.slippage_bps,
                platform_fee_bps: fee.map_or(0, |fee| fee.fee.basis_points()),
                transaction_version: TRANSACTION_VERSION,
                sponsored_swap: sponsored.then_some(true),
                sponsor_exec: sponsored.then_some(false),
                platform_fee_mode: fee.map(|fee| fee.mode()),
            })
            .await?;
        Ok((raw, response.quote(trade, fee)?))
    }

    pub(crate) async fn prepare_with_fee(
        &self,
        trade: &Trade,
        payer: Option<&Pubkey>,
        fee: Option<ProviderFee>,
    ) -> Result<PreparedSwap> {
        let (quote_response, quote) = self.fetch_quote(trade, payer, fee).await?;
        let response = self
            .api
            .swap_instructions(&InstructionsRequest {
                quote_response,
                user_public_key: trade.wallet.to_string(),
                wrap_and_unwrap_sol: true,
                transaction_version: TRANSACTION_VERSION,
                dynamic_compute_unit_limit: false,
                compute_unit_price_micro_lamports: 0,
                sponsor: payer.map(ToString::to_string),
                sponsor_exec: payer.map(|_| false),
                fee_account: fee.map(|fee| fee.account().to_string()),
            })
            .await?;
        let mut instructions = response.instructions(trade.wallet, payer)?;
        if let Some(fee) = fee {
            instructions.insert(0, fee.setup(payer.unwrap_or(&trade.wallet)));
        }
        let addresses = response
            .address_lookup_table_addresses
            .iter()
            .map(|address| address.parse().map_err(decode_error))
            .collect::<Result<Vec<_>>>()?;
        Ok(PreparedSwap {
            venue: VENUE,
            quote,
            instructions,
            lookup_tables: load_address_lookup_tables(&self.rpc, &addresses).await?,
            format: TransactionFormat::V0,
        })
    }
}

#[async_trait]
impl Dex for DFlow {
    fn name(&self) -> &'static str {
        VENUE
    }

    async fn quote(&self, trade: &Trade) -> anyhow::Result<Quote> {
        Ok(self.fetch_quote(trade, None, None).await?.1)
    }

    async fn prepare_swap(&self, trade: &Trade) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_fee(trade, None, None).await?)
    }

    async fn prepare_sponsored_swap(
        &self,
        trade: &Trade,
        payer: &Pubkey,
    ) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_fee(trade, Some(payer), None).await?)
    }
}

impl QuoteResponse {
    fn quote(&self, trade: &Trade, fee: Option<ProviderFee>) -> Result<Quote> {
        let (input, output) = route_mints(trade);
        let min_out = parse_amount(&self.other_amount_threshold)?;
        if self.input_mint != input.to_string()
            || self.output_mint != output.to_string()
            || self.slippage_bps != trade.slippage_bps
            || self.route_plan.is_empty()
            || parse_amount(&self.min_out_amount)? != min_out
        {
            return Err(decode_error("quote does not match the requested trade"));
        }
        let mut quote = validated_quote(
            VENUE,
            trade,
            parse_amount(&self.in_amount)?,
            parse_amount(&self.out_amount)?,
            min_out,
        )?;
        match (fee, &self.platform_fee) {
            (Some(requested), Some(received)) => {
                let amount = parse_amount(&received.amount)?;
                if received
                    .fee_account
                    .as_ref()
                    .is_some_and(|account| *account != requested.account().to_string())
                {
                    return Err(decode_error("unexpected platform fee account"));
                }
                requested.validate_amount(&quote, amount, received.fee_bps)?;
                quote.application_fee = amount;
            }
            (Some(_), None) => return Err(decode_error("missing requested platform fee")),
            (None, Some(received))
                if received.fee_bps != 0 || parse_amount(&received.amount)? != 0 =>
            {
                return Err(decode_error(
                    "unexpected platform fee; SDK fee is applied separately",
                ));
            }
            _ => {}
        }
        Ok(quote)
    }
}

impl InstructionsResponse {
    fn instructions(&self, wallet: Pubkey, payer: Option<&Pubkey>) -> Result<Vec<Instruction>> {
        if self.transaction_version != TRANSACTION_VERSION {
            return Err(decode_error("only v0 swap instructions are supported"));
        }
        let mut instructions = Vec::new();
        for api_instruction in self
            .compute_budget_instructions
            .iter()
            .chain(&self.setup_instructions)
            .chain(std::iter::once(&self.swap_instruction))
            .chain(&self.cleanup_instructions)
            .chain(&self.other_instructions)
        {
            let instruction = api_instruction.decode_base64(VENUE, wallet, payer)?;
            if retain_compute_budget(VENUE, &instruction)? {
                instructions.push(instruction);
            }
        }
        if instructions.is_empty() || self.swap_instruction.data.is_empty() {
            return Err(decode_error("missing swap instruction"));
        }
        Ok(instructions)
    }
}

fn parse_amount(value: &str) -> Result<u64> {
    value.parse().map_err(decode_error)
}
