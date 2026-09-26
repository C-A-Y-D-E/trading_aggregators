//! Robinhood mainnet swaps through Relay or Uniswap. The trading wallet pays its own gas.

pub use crate::UsdValue;
pub use crate::evm::{
    Address, Amount, AppFee, BLOXROUTE_DEFAULT_URL, BloxrouteSubmitter, Bytes, Currency, EthPool,
    FeeAmount, NativeMarket, PoolKey, PoolRef, PreparedSwap, Quote, QuoteSource, RpcSubmitter,
    Signature, Signer, Submitter, SwapError, SwapResult, Trade, Transaction, TransactionHash,
    TxEip1559, UniswapDeployment, UniswapPool, UnsignedTransaction,
};

use alloy_primitives::address;

pub const CHAIN_ID: u64 = 4663;

#[derive(Debug, Clone, Copy)]
pub struct Robinhood;

impl crate::evm::sealed::Sealed for Robinhood {}

impl crate::evm::Network for Robinhood {
    const CHAIN_ID: u64 = CHAIN_ID;
    const NAME: &'static str = "Robinhood";
    // Relay's contracts on this chain, from its /chains API; swaps fail if Relay redeploys them.
    const ROUTER: Address = address!("0xb92fe925dc43a0ecde6c8b1a2709c170ec4fff4f");
    const APPROVAL_PROXY: Address = address!("0xccc88a9d1b4ed6b0eaba998850414b24f1c315be");
    // Uniswap's official Robinhood Chain deployments.
    const UNISWAP: UniswapDeployment = UniswapDeployment {
        weth: address!("0x0Bd7D308f8E1639FAb988df18A8011f41EAcAD73"),
        // Our CswapRouter; it takes the app fee on every Uniswap swap.
        cswap_router: address!("0xd8fbb0ded86fb6d593b598a44daa082deb329ade"),
        v2_factory: address!("0x8bceaa40b9acdfaedf85adf4ff01f5ad6517937f"),
        v3_factory: address!("0x1f7d7550b1b028f7571e69a784071f0205fd2efa"),
        v3_quoter: address!("0x33e885ed0ec9bf04ecfb19341582aadcb4c8a9e7"),
        v4_quoter: address!("0x8dc178efb8111bb0973dd9d722ebeff267c98f94"),
        v4_state_view: address!("0xf3334192d15450cdd385c8b70e03f9a6bd9e673b"),
        v4_position_manager: address!("0x58daec3116aae6d93017baaea7749052e8a04fa7"),
        // Deepest v3 WETH/USDG pool (0.01%); USDG is Robinhood's stablecoin, with 6 decimals.
        eth_usd_pool: address!("0x52e65B17fB6E5BA00Ed806f37Afcd2DaA50271Ca"),
        usd_decimals: 6,
    };
}

pub type Client = crate::evm::Client<Robinhood>;
