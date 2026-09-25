use serde::{Deserialize, Serialize};

use super::ApiInstruction;
use crate::Result;
use crate::http::ApiClient;

pub const DEFAULT_BASE_URL: &str = "https://api.blox.ag";

pub(crate) const VENUE: &str = "bloxroute";
const INSTRUCTIONS_PATH: &str = "/v1/swap-instructions";
const AUTH_HEADER: &str = "Authorization";

/// bloXroute's aggregator API; separate from `BloxrouteSubmitter`.
pub(crate) struct BloxrouteApi {
    api: ApiClient,
}

impl BloxrouteApi {
    pub fn new() -> Self {
        Self {
            api: ApiClient::new(VENUE, DEFAULT_BASE_URL),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.api = self.api.with_base_url(base_url);
        self
    }

    pub fn with_auth_header(mut self, auth_header: impl Into<String>) -> Self {
        self.api = self.api.with_authentication(AUTH_HEADER, auth_header);
        self
    }

    pub async fn swap_instructions(
        &self,
        request: &InstructionsRequest,
    ) -> Result<InstructionsResponse> {
        let url = format!("{}{INSTRUCTIONS_PATH}", self.api.base_url);
        self.api
            .request(self.api.http.get(url).query(request))
            .await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionsRequest {
    pub input_mint: String,
    pub output_mint: String,
    pub amount: u64,
    pub payer: String,
    pub slippage_bps: u64,
    pub wrap_unwrap_sol: bool,
    pub priority_fee: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_fee_bps: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_fee_mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_fee_account: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionsResponse {
    pub input_mint: String,
    pub output_mint: String,
    pub input_amount: u64,
    pub output_amount: u64,
    pub output_amount_min: u64,
    pub input_value_usd: Option<f64>,
    pub output_value_usd: Option<f64>,
    pub platform_fee: Option<PlatformFee>,
    pub transaction_config: TransactionConfig,
    pub setup_instructions: Vec<ApiInstruction>,
    pub swap_instruction: ApiInstruction,
}

#[derive(Deserialize)]
pub(crate) struct PlatformFee {
    pub amount: u64,
    pub bps: u16,
    pub mint: Option<String>,
    pub mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TransactionConfig {
    pub compute_unit_limit: u32,
    pub loaded_accounts_data_size_limit: u32,
    pub heap_size: Option<u32>,
    pub priority_fee: Option<u64>,
}
