//! Shared transaction support behind named chain clients such as [`crate::robinhood::Client`].

use std::{marker::PhantomData, time::Duration};

use crate::{Result, TradeError};
use dexes::uniswap::Uniswap;
use sources::relay::Relay;

mod dexes;
mod execution;
mod node;
pub mod robinhood;
mod sources;
mod submit;
mod types;
mod unsigned;

pub use alloy_consensus::TxEip1559;
pub use alloy_primitives::{Address, B256 as TransactionHash, Bytes, Signature, U256 as Amount};
pub use dexes::uniswap::{EthPool, NativeMarket, PoolKey, PoolRef, UniswapDeployment, UniswapPool};
pub use submit::{BLOXROUTE_DEFAULT_URL, BloxrouteSubmitter, RpcSubmitter};
pub use types::{
    AppFee, Currency, FeeAmount, PreparedSwap, Quote, QuoteSource, Signer, Submitter, SwapError,
    SwapResult, Trade, Transaction,
};
pub use unsigned::UnsignedTransaction;

/// Used only when neither the trade nor the client names a source.
const DEFAULT_QUOTE_SOURCE: QuoteSource = QuoteSource::Relay;

pub(crate) mod sealed {
    pub trait Sealed {}
}

pub trait Network: sealed::Sealed + Send + Sync {
    const CHAIN_ID: u64;
    const NAME: &'static str;
    const ROUTER: Address;
    const APPROVAL_PROXY: Address;
    const UNISWAP: UniswapDeployment;
}

pub struct Client<C: Network> {
    relay: Relay,
    uniswap: Uniswap,
    node: node::Node,
    quote_source: Option<QuoteSource>,
    app_fee: Option<AppFee>,
    usd_value_enabled: bool,
    confirmation_timeout: Duration,
    network: PhantomData<C>,
}

impl<C: Network> Client<C> {
    /// `rpc_url` is this chain's node; Relay uses its production URL.
    pub fn new(rpc_url: &str) -> Result<Self> {
        Ok(Self {
            relay: Relay::new(),
            uniswap: Uniswap::default(),
            node: node::Node::new(rpc_url)?,
            quote_source: None,
            app_fee: None,
            usd_value_enabled: true,
            confirmation_timeout: Duration::from_secs(60),
            network: PhantomData,
        })
    }

    /// Your deployed `CswapRouter`; Uniswap swaps go through it and it takes the app fee.
    pub fn with_uniswap_router(mut self, router: Address) -> Self {
        self.uniswap = self.uniswap.with_router(router);
        self
    }

    pub fn with_quote_source(mut self, source: QuoteSource) -> Self {
        self.quote_source = Some(source);
        self
    }

    /// Precedence: trade, then client, then Relay.
    pub fn quote_source_for(&self, trade: &Trade) -> QuoteSource {
        trade
            .quote_source
            .or(self.quote_source)
            .unwrap_or(DEFAULT_QUOTE_SOURCE)
    }

    /// The deepest Uniswap ETH pool for `token`; `None` means use Relay.
    pub async fn find_native_market(&self, token: Address) -> Result<Option<NativeMarket>> {
        self.uniswap.find_market::<C>(&self.node, token).await
    }

    /// Whether a user-supplied pool is a Uniswap ETH pool the router can trade; `None` means
    /// use Relay. On `Some`, trade `token` with `with_pool(pool)`.
    pub async fn eth_pool(&self, pool: PoolRef) -> Result<Option<EthPool>> {
        self.uniswap.eth_pool::<C>(&self.node, pool).await
    }

    pub fn chain_id(&self) -> u64 {
        C::CHAIN_ID
    }

    pub fn with_relay_url(mut self, base_url: impl Into<String>) -> Self {
        self.relay = self.relay.with_base_url(base_url);
        self
    }

    pub fn with_relay_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.relay = self.relay.with_api_key(api_key);
        self
    }

    pub fn with_app_fee(mut self, fee: AppFee) -> Self {
        self.app_fee = (fee.basis_points() > 0).then_some(fee);
        self
    }

    /// `quote.usd_value` is then always `None`.
    pub fn without_usd_value(mut self) -> Self {
        self.usd_value_enabled = false;
        self
    }

    pub fn with_confirmation_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(TradeError::Build(
                "confirmation timeout must be positive".into(),
            ));
        }
        self.confirmation_timeout = timeout;
        Ok(self)
    }

    pub async fn quote(&self, trade: &Trade) -> Result<Quote> {
        Ok(self.prepare_swap(trade).await?.quote)
    }

    pub async fn prepare_swap(&self, trade: &Trade) -> Result<PreparedSwap> {
        let mut prepared = match self.quote_source_for(trade) {
            QuoteSource::Relay => self.relay.prepare::<C>(trade, self.app_fee).await?,
            QuoteSource::Uniswap => {
                self.uniswap
                    .prepare::<C>(&self.node, trade, self.app_fee)
                    .await?
            }
        };
        if !self.usd_value_enabled {
            prepared.quote.usd_value = None;
        }
        Ok(prepared)
    }

    pub async fn swap(
        &self,
        trade: &Trade,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
    ) -> std::result::Result<SwapResult, SwapError> {
        let prepared = self
            .prepare_swap(trade)
            .await
            .map_err(SwapError::before_submission)?;
        self.execute_swap(prepared, signer, submitter).await
    }
}
