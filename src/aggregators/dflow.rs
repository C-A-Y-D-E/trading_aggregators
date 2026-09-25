use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ApiInstruction;
use crate::http::ApiClient;
use crate::{Result, TradeError};

pub const DEFAULT_BASE_URL: &str = "https://quote-api.dflow.net";

pub(crate) const VENUE: &str = "dflow";
pub(crate) const TRANSACTION_VERSION: &str = "v0";
const QUOTE_PATH: &str = "/quote";
const INSTRUCTIONS_PATH: &str = "/swap-instructions";
const API_KEY_HEADER: &str = "x-api-key";

pub(crate) struct DFlowApi {
    api: ApiClient,
}

impl DFlowApi {
    pub fn new() -> Self {
        Self {
            api: ApiClient::new(VENUE, DEFAULT_BASE_URL),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.api = self.api.with_base_url(base_url);
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api = self.api.with_authentication(API_KEY_HEADER, api_key);
        self
    }

    /// Returns the raw quote too, because `/swap-instructions` expects it back unchanged.
    pub async fn quote(&self, request: &QuoteRequest) -> Result<(Value, QuoteResponse)> {
        let url = format!("{}{QUOTE_PATH}", self.api.base_url);
        let raw: Value = self
            .api
            .request(self.api.http.get(url).query(request))
            .await?;
        let quote = serde_json::from_value(raw.clone()).map_err(decode_error)?;
        Ok((raw, quote))
    }

    pub async fn swap_instructions(
        &self,
        request: &InstructionsRequest,
    ) -> Result<InstructionsResponse> {
        let url = format!("{}{INSTRUCTIONS_PATH}", self.api.base_url);
        self.api
            .request(self.api.http.post(url).json(request))
            .await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuoteRequest {
    pub input_mint: String,
    pub output_mint: String,
    pub amount: u64,
    pub slippage_bps: u64,
    pub platform_fee_bps: u16,
    pub transaction_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsored_swap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsor_exec: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_fee_mode: Option<&'static str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuoteResponse {
    pub input_mint: String,
    pub output_mint: String,
    pub in_amount: String,
    pub out_amount: String,
    pub min_out_amount: String,
    pub other_amount_threshold: String,
    pub slippage_bps: u64,
    pub platform_fee: Option<PlatformFee>,
    pub route_plan: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlatformFee {
    pub amount: String,
    pub fee_bps: u64,
    pub fee_account: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionsRequest {
    pub quote_response: Value,
    pub user_public_key: String,
    pub wrap_and_unwrap_sol: bool,
    pub transaction_version: &'static str,
    pub dynamic_compute_unit_limit: bool,
    pub compute_unit_price_micro_lamports: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsor_exec: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_account: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionsResponse {
    pub transaction_version: String,
    pub compute_budget_instructions: Vec<ApiInstruction>,
    pub setup_instructions: Vec<ApiInstruction>,
    pub swap_instruction: ApiInstruction,
    pub cleanup_instructions: Vec<ApiInstruction>,
    pub other_instructions: Vec<ApiInstruction>,
    pub address_lookup_table_addresses: Vec<String>,
}

pub(crate) fn decode_error(error: impl std::fmt::Display) -> TradeError {
    TradeError::Decode(VENUE, error.to_string())
}
