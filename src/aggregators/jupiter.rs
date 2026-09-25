use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ApiInstruction;
use crate::Result;
use crate::http::ApiClient;

pub const DEFAULT_BASE_URL: &str = "https://api.jup.ag";

pub(crate) const VENUE: &str = "jupiter";
const BUILD_PATH: &str = "/swap/v2/build";
const API_KEY_HEADER: &str = "x-api-key";

pub(crate) struct JupiterApi {
    api: ApiClient,
}

impl JupiterApi {
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

    pub async fn build(&self, request: &BuildRequest) -> Result<BuildResponse> {
        let url = format!("{}{BUILD_PATH}", self.api.base_url);
        self.api
            .request(self.api.http.get(url).query(request))
            .await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildRequest {
    pub input_mint: String,
    pub output_mint: String,
    pub amount: u64,
    pub taker: String,
    pub slippage_bps: u64,
    pub wrap_and_unwrap_sol: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildResponse {
    pub input_mint: String,
    pub output_mint: String,
    pub swap_mode: String,
    pub in_amount: String,
    pub out_amount: String,
    pub other_amount_threshold: String,
    pub setup_instructions: Vec<ApiInstruction>,
    pub swap_instruction: ApiInstruction,
    pub cleanup_instruction: Option<ApiInstruction>,
    pub other_instructions: Vec<ApiInstruction>,
    pub addresses_by_lookup_table_address: Option<HashMap<String, Vec<String>>>,
}
