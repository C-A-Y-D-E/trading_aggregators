use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use solana_instruction::Instruction;

use super::{move_rent_to_sponsor, retain_compute_budget, validated_quote};
use crate::aggregators::relay::{
    Details, QuoteRequest, QuoteResponse, RelayApi, Swap, VENUE, decode_error,
};
use crate::aggregators::{ApiInstruction, TransactionConfig};
use crate::error::Result;
use crate::solana::provider_fee::ProviderFee;
use crate::{
    Dex, PreparedSwap, Pubkey, Quote, Settlement, Side, Trade, TransactionFormat, USDC_MINT,
};

const SOLANA_CHAIN_ID: u64 = 792_703_809;
const NATIVE_SOL: &str = "11111111111111111111111111111111";
const V1_VERSION: u8 = 1;

mod app_fee;

/// One atomic, same-chain Solana swap, always as a V1 transaction: up to 4,096 bytes and
/// 64 accounts, with no lookup tables. Solver deposits and multi-transaction flows are rejected.
pub struct Relay {
    api: RelayApi,
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
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

    pub(crate) async fn prepare_with_fee(
        &self,
        trade: &Trade,
        payer: Option<&Pubkey>,
        fee: Option<ProviderFee>,
    ) -> Result<PreparedSwap> {
        trade.validate()?;
        let swap = swap(trade);
        let app_fees = fee
            .map(|fee| app_fee::request(trade, fee))
            .transpose()?
            .into_iter()
            .collect();
        let options = SolanaOptions {
            include_compute_unit_limit: false,
            deposit_fee_payer: payer.map(ToString::to_string),
            use_v1_transaction: true,
        };
        let response: QuoteResponse<String, TransactionData> = self
            .api
            .quote(&QuoteRequest::new(&swap, options, app_fees))
            .await?;
        let mut quote = quote(&response.details, &swap, trade)?;
        app_fee::apply(&response.fees, trade, fee, &mut quote)?;
        let data = swap_data(&response)?;
        Ok(PreparedSwap {
            venue: VENUE,
            quote,
            instructions: data.instructions(trade, payer)?,
            lookup_tables: vec![],
            format: TransactionFormat::V1,
        })
    }
}

#[async_trait]
impl Dex for Relay {
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
        payer: &Pubkey,
    ) -> anyhow::Result<PreparedSwap> {
        Ok(self.prepare_with_fee(trade, Some(payer), None).await?)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SolanaOptions {
    include_compute_unit_limit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    deposit_fee_payer: Option<String>,
    use_v1_transaction: bool,
}

fn swap(trade: &Trade) -> Swap<String> {
    let (input, output) = currencies(trade);
    Swap {
        wallet: trade.wallet.to_string(),
        chain_id: SOLANA_CHAIN_ID,
        input,
        output,
        amount: trade.amount.to_string(),
        slippage_bps: trade.slippage_bps.to_string(),
    }
}

fn currencies(trade: &Trade) -> (String, String) {
    let settlement = match trade.settlement {
        Settlement::Sol => NATIVE_SOL.to_owned(),
        Settlement::Usdc => USDC_MINT.to_string(),
    };
    match trade.side {
        Side::Buy => (settlement, trade.mint.to_string()),
        Side::Sell => (trade.mint.to_string(), settlement),
    }
}

fn quote(details: &Details<String>, swap: &Swap<String>, trade: &Trade) -> Result<Quote> {
    if !details.matches(swap) {
        return Err(decode_error(
            "quote does not match the requested wallet, currencies or chain",
        ));
    }
    let mut quote = validated_quote(
        VENUE,
        trade,
        details.currency_in.amount.parse().map_err(decode_error)?,
        details.currency_out.amount.parse().map_err(decode_error)?,
        details
            .currency_out
            .minimum_amount
            .parse()
            .map_err(decode_error)?,
    )?;
    quote.usd_value = details.usd_value();
    Ok(quote)
}

fn swap_data(response: &QuoteResponse<String, TransactionData>) -> Result<&TransactionData> {
    let [step] = response.steps.as_slice() else {
        return Err(decode_error("expected one atomic swap step"));
    };
    let [item] = step.items.as_slice() else {
        return Err(decode_error("expected one swap transaction"));
    };
    // Deposits may settle asynchronously even on the same chain; submitting them is not an atomic swap.
    if step.id != "swap" || step.kind != "transaction" || item.status != "incomplete" {
        return Err(decode_error(
            "only an unexecuted same-chain swap transaction is supported",
        ));
    }
    item.data
        .as_ref()
        .ok_or_else(|| decode_error("missing executable swap data"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionData {
    chain_id: Option<u64>,
    instructions: Vec<ApiInstruction>,
    transaction_version: Option<u8>,
    #[serde(default)]
    transaction_config: TransactionConfig,
}

impl TransactionData {
    fn instructions(&self, trade: &Trade, payer: Option<&Pubkey>) -> Result<Vec<Instruction>> {
        if self.chain_id.is_some_and(|id| id != SOLANA_CHAIN_ID) {
            return Err(decode_error("transaction is not on Solana"));
        }
        if self.transaction_version != Some(V1_VERSION) {
            return Err(decode_error(
                "transaction version does not match the request",
            ));
        }
        let mut instructions = Vec::new();
        for api_instruction in &self.instructions {
            let mut instruction = api_instruction.decode_hex(VENUE, trade.wallet, payer)?;
            if let Some(payer) = payer {
                move_rent_to_sponsor(VENUE, &mut instruction, trade.wallet, *payer)?;
            }
            if retain_compute_budget(VENUE, &instruction)? {
                instructions.push(instruction);
            }
        }
        if instructions.is_empty()
            || !instructions
                .iter()
                .flat_map(|ix| &ix.accounts)
                .any(|account| account.is_signer && account.pubkey == trade.wallet)
        {
            return Err(decode_error(
                "missing swap instructions or trading wallet signer",
            ));
        }
        instructions.extend(self.transaction_config.instructions(VENUE)?);
        Ok(instructions)
    }
}
