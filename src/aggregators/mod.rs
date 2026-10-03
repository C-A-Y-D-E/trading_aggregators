//! Provider HTTP APIs: requests and response types only. `solana::sources` and `evm::sources`
//! turn these responses into each chain's transactions.

use serde::Deserialize;

pub mod bloxroute;
pub mod dflow;
pub mod jupiter;
pub mod relay;

/// A Solana instruction as providers return it, still encoded.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiInstruction {
    pub program_id: String,
    #[serde(alias = "keys")]
    pub accounts: Vec<ApiAccount>,
    pub data: String,
}

/// Budget settings a provider wants in a V1 transaction's header.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TransactionConfig {
    pub compute_unit_limit: Option<u32>,
    pub loaded_accounts_data_size_limit: Option<u32>,
    pub heap_size: Option<u32>,
    pub priority_fee: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiAccount {
    pub pubkey: String,
    pub is_signer: bool,
    pub is_writable: bool,
}
