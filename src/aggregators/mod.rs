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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiAccount {
    pub pubkey: String,
    pub is_signer: bool,
    pub is_writable: bool,
}
