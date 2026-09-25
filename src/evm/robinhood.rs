//! Robinhood mainnet swaps through Relay. The trading wallet pays its own gas.

pub use crate::UsdValue;
pub use crate::evm::{
    Address, Amount, AppFee, BLOXROUTE_DEFAULT_URL, BloxrouteSubmitter, Bytes, Currency, FeeAmount,
    PreparedSwap, Quote, RpcSubmitter, Signature, Signer, Submitter, SwapError, SwapResult, Trade,
    Transaction, TransactionHash, TxEip1559, UnsignedTransaction,
};

pub const CHAIN_ID: u64 = 4663;

#[derive(Debug, Clone, Copy)]
pub struct Robinhood;

impl crate::evm::sealed::Sealed for Robinhood {}

impl crate::evm::Network for Robinhood {
    const CHAIN_ID: u64 = CHAIN_ID;
    const NAME: &'static str = "Robinhood";
    // Relay's contracts on this chain, from its /chains API; swaps fail if Relay redeploys them.
    const ROUTER: Address =
        alloy_primitives::address!("0xb92fe925dc43a0ecde6c8b1a2709c170ec4fff4f");
    const APPROVAL_PROXY: Address =
        alloy_primitives::address!("0xccc88a9d1b4ed6b0eaba998850414b24f1c315be");
}

pub type Client = crate::evm::Client<Robinhood>;
