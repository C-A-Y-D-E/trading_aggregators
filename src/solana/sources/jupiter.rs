use async_trait::async_trait;
use solana_instruction::Instruction;
use solana_message::AddressLookupTableAccount;
use solana_pubkey::Pubkey;

use super::{route_mints, validated_quote};
use crate::aggregators::jupiter::{BuildRequest, BuildResponse, JupiterApi, VENUE};
use crate::error::{Result, TradeError};
use crate::solana::types::{Dex, PreparedSwap, Quote, Trade, TransactionFormat};

const EXACT_INPUT_MODE: &str = "ExactIn";

pub struct Jupiter {
    api: JupiterApi,
}

impl Default for Jupiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Jupiter {
    pub fn new() -> Self {
        Self {
            api: JupiterApi::new(),
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

    async fn build(&self, trade: &Trade, payer: Option<&Pubkey>) -> Result<BuildResponse> {
        trade.validate()?;
        let (input, output) = route_mints(trade);
        self.api
            .build(&BuildRequest {
                input_mint: input.to_string(),
                output_mint: output.to_string(),
                amount: trade.amount,
                taker: trade.wallet.to_string(),
                slippage_bps: trade.slippage_bps,
                wrap_and_unwrap_sol: true,
                payer: payer.map(ToString::to_string),
            })
            .await
    }

    async fn prepare_with_payer(
        &self,
        trade: &Trade,
        payer: Option<&Pubkey>,
    ) -> Result<PreparedSwap> {
        let build = self.build(trade, payer).await?;
        Ok(PreparedSwap {
            venue: VENUE,
            quote: build.quote(trade)?,
            instructions: build.instructions(trade.wallet, payer)?,
            lookup_tables: build.lookup_tables()?,
            format: TransactionFormat::V0,
        })
    }
}

#[async_trait]
impl Dex for Jupiter {
    fn name(&self) -> &'static str {
        VENUE
    }

    async fn quote(&self, trade: &Trade) -> anyhow::Result<Quote> {
        Ok(self.build(trade, None).await?.quote(trade)?)
    }

    async fn prepare_swap(&self, trade: &Trade) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_payer(trade, None).await?)
    }

    async fn prepare_sponsored_swap(
        &self,
        trade: &Trade,
        payer: &Pubkey,
    ) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_payer(trade, Some(payer)).await?)
    }
}

impl BuildResponse {
    fn quote(&self, trade: &Trade) -> Result<Quote> {
        let (input, output) = route_mints(trade);
        if self.swap_mode != EXACT_INPUT_MODE
            || self.input_mint != input.to_string()
            || self.output_mint != output.to_string()
        {
            return Err(decode_error(
                "build response does not match the requested exact-input trade",
            ));
        }
        validated_quote(
            VENUE,
            trade,
            parse_amount("inAmount", &self.in_amount)?,
            parse_amount("outAmount", &self.out_amount)?,
            parse_amount("otherAmountThreshold", &self.other_amount_threshold)?,
        )
    }

    fn instructions(&self, wallet: Pubkey, payer: Option<&Pubkey>) -> Result<Vec<Instruction>> {
        self.setup_instructions
            .iter()
            .chain(std::iter::once(&self.swap_instruction))
            .chain(&self.cleanup_instruction)
            .chain(&self.other_instructions)
            .map(|instruction| instruction.decode_base64(VENUE, wallet, payer))
            .collect()
    }

    fn lookup_tables(&self) -> Result<Vec<AddressLookupTableAccount>> {
        let Some(tables) = &self.addresses_by_lookup_table_address else {
            return Ok(vec![]);
        };
        tables
            .iter()
            .map(|(key, addresses)| {
                Ok(AddressLookupTableAccount {
                    key: parse_address(key)?,
                    addresses: addresses
                        .iter()
                        .map(|address| parse_address(address))
                        .collect::<Result<_>>()?,
                })
            })
            .collect()
    }
}

fn parse_amount(field: &str, value: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| decode_error(format!("bad {field} {value:?}")))
}

fn parse_address(value: &str) -> Result<Pubkey> {
    value
        .parse()
        .map_err(|_| decode_error(format!("bad lookup table address {value}")))
}

fn decode_error(error: impl std::fmt::Display) -> TradeError {
    TradeError::Decode(VENUE, error.to_string())
}
