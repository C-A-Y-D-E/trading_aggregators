#![doc = include_str!("../README.md")]

pub mod aggregators;
pub mod error;
pub mod evm;
mod http;
pub mod solana;
mod trading_aggregator;
mod usd_value;

pub use error::{Result, TradeError};
pub use evm::robinhood;
pub use trading_aggregator::TradingAggregator;
pub use usd_value::UsdValue;

// Solana names stay at the root so existing `trading_aggregator::Trade`-style imports keep working.
pub use solana::{
    AddressLookupTableAccount, Bloxroute, BloxrouteSubmitter, DEFAULT_SOL_USDC_POOL, DFlow, Dex,
    FeeCollection, GasSponsor, Jupiter, NativeMarket, PreparedSwap, Pubkey, PumpFun, PumpSwap,
    Quote, QuoteSource, Relay, RpcClient, RpcSubmitter, SdkFee, Settlement, Side, Signature,
    Signer, SolUsdcPrice, SolUsdcPriceSource, SubmitProtection, Submitter, SwapResult, SwapStatus,
    Tip, Trade, TradingClient, USDC_MINT, Venue, VersionedTransaction, load_address_lookup_table,
    load_address_lookup_tables, shared_lookup_addresses,
};
