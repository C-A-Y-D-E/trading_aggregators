//! Solana trading, including source selection, SOL/USDC fees and optional sponsorship.

pub mod client;
pub mod dexes;
mod executor;
pub mod gas_sponsor;
pub mod lookup_table;
mod provider_fee;
pub mod sdk_fee;
mod sources;
pub mod submit;
pub mod types;

pub use client::TradingClient;
pub use client::TradingClient as Client;
pub use dexes::pumpfun::PumpFun;
pub use dexes::pumpswap::{DEFAULT_SOL_USDC_POOL, PumpSwap, USDC_MINT};
pub use gas_sponsor::{GasSponsor, SolUsdcPrice, SolUsdcPriceSource};
pub use lookup_table::{
    load_address_lookup_table, load_address_lookup_tables, shared_lookup_addresses,
};
pub use sdk_fee::{FeeCollection, SdkFee};
pub use sources::{bloxroute::Bloxroute, dflow::DFlow, jupiter::Jupiter, relay::Relay};
pub use submit::{BloxrouteSubmitter, RpcSubmitter, SubmitProtection};
pub use types::{
    Dex, NativeMarket, PreparedSwap, Quote, QuoteSource, Settlement, Side, Signer, Submitter,
    SwapResult, SwapStatus, Tip, Trade, TransactionFormat, Venue,
};

pub use crate::UsdValue;
pub use solana_client::nonblocking::rpc_client::RpcClient;
pub use solana_message::AddressLookupTableAccount;
pub use solana_pubkey::Pubkey;
pub use solana_signature::Signature;
pub use solana_transaction::versioned::VersionedTransaction;
