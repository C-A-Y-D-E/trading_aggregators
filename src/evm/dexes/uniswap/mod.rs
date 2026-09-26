//! Uniswap v2/v3/v4 swaps of ETH pairs through our fee-taking `CswapRouter`, one pool per trade.

use std::future::Future;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::B256;
use alloy_sol_types::SolCall;
use tokio::task::JoinSet;

use super::super::node::Node;
use super::super::types::{QuoteSource, Trade};
use super::super::{Address, Amount, AppFee, Currency, FeeAmount, Network, PreparedSwap, Quote};
use crate::{Result, TradeError, UsdValue};

mod abi;
mod router;
mod v2;
mod v3;
mod v4;

const VENUE: &str = "uniswap";
/// The router rejects the swap after this; the executor also refuses quotes older than 60 s.
const DEADLINE: Duration = Duration::from_secs(120);
const Q96: f64 = 79_228_162_514_264_337_593_543_950_336.0;
/// 0.1 ETH: large enough that thin pools rank below deep ones.
const DISCOVERY_PROBE_WEI: u64 = 100_000_000_000_000_000;
const ADDRESS_HEX_LEN: usize = 2 + 2 * 20;
const POOL_ID_HEX_LEN: usize = 2 + 2 * 32;

/// One Uniswap pool pairing a token with ETH (native in v4, or WETH).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniswapPool {
    V2 {
        pair: Address,
    },
    V3 {
        pool: Address,
    },
    /// Hookless pools only: hooks can charge fees or revert outside the quote.
    V4 {
        key: PoolKey,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolKey {
    pub currency0: Address,
    pub currency1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hooks: Address,
}

/// The deepest ETH pool for a token. Pass `pool` to the trade with `QuoteSource::Uniswap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeMarket {
    pub source: QuoteSource,
    pub pool: UniswapPool,
}

/// A pool as users and explorers identify it: a v2/v3 address, or a v4 pool id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolRef {
    Address(Address),
    V4Id(B256),
}

impl FromStr for PoolRef {
    type Err = TradeError;

    /// Accepts `0x` + 40 hex digits (an address) or `0x` + 64 hex digits (a v4 pool id).
    fn from_str(value: &str) -> Result<Self> {
        let invalid =
            || TradeError::Build(format!("{value:?} is not a pool address or v4 pool id"));
        match value.len() {
            ADDRESS_HEX_LEN => value.parse().map(Self::Address).map_err(|_| invalid()),
            POOL_ID_HEX_LEN => value.parse().map(Self::V4Id).map_err(|_| invalid()),
            _ => Err(invalid()),
        }
    }
}

/// An official Uniswap pool pairing `token` with ETH that `QuoteSource::Uniswap` can trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthPool {
    pub token: Address,
    pub pool: UniswapPool,
}

/// Uniswap's contracts on one chain, plus the pool that prices ETH in USD.
#[derive(Debug, Clone, Copy)]
pub struct UniswapDeployment {
    pub weth: Address,
    /// Our fee-taking router, not a Uniswap contract.
    pub cswap_router: Address,
    pub v2_factory: Address,
    pub v3_factory: Address,
    pub v3_quoter: Address,
    pub v4_quoter: Address,
    pub v4_state_view: Address,
    /// Resolves a v4 pool id back to its key.
    pub v4_position_manager: Address,
    /// A deep v3 WETH/stablecoin pool.
    pub eth_usd_pool: Address,
    pub usd_decimals: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// ETH in, token out.
    Buy,
    /// Token in, ETH out.
    Sell,
}

/// What one pool says about a swap before it happens.
struct PoolQuote {
    amount_out: Amount,
    /// Wei per token base unit at the pre-trade price.
    eth_per_token: f64,
}

pub(crate) struct Uniswap;

impl Uniswap {
    /// `Some` when `pool` is an official Uniswap pool pairing a token with ETH that the router
    /// can trade (hookless for v4); `None` means use Relay.
    pub async fn eth_pool<C: Network>(
        &self,
        node: &Node,
        pool: PoolRef,
    ) -> Result<Option<EthPool>> {
        let deployment = C::UNISWAP;
        match pool {
            PoolRef::Address(address) => address_eth_pool(node, &deployment, address).await,
            PoolRef::V4Id(id) => v4::eth_pool(node, &deployment, id).await,
        }
    }

    /// `None` when the token has no usable ETH pool on v2, v3 or hookless v4; use Relay then.
    ///
    /// Pools are ranked by what a fixed buy returns rather than by reserves: that accounts for
    /// the fee tier and in-range liquidity, and pools stuck at a price bound fail their quote.
    pub async fn find_market<C: Network>(
        &self,
        node: &Node,
        token: Address,
    ) -> Result<Option<NativeMarket>> {
        let deployment = C::UNISWAP;
        let (v2, v3, v4) = tokio::join!(
            v2::find(node, &deployment, token),
            v3::find(node, &deployment, token),
            v4::find(node, &deployment, token)
        );
        let probes = v2?.into_iter().chain(v3?).chain(v4?).map(|pool| {
            let node = node.clone();
            async move {
                let probe = Amount::from(DISCOVERY_PROBE_WEI);
                let route = Route {
                    pool,
                    token,
                    direction: Direction::Buy,
                };
                let quoted = quote(&node, &deployment, route, probe).await;
                quoted.ok().map(|quoted| (pool, quoted.amount_out))
            }
        });
        let deepest = in_parallel(probes)
            .await
            .into_iter()
            .flatten()
            .max_by_key(|(_, amount_out)| *amount_out);
        Ok(deepest.map(|(pool, _)| NativeMarket {
            source: QuoteSource::Uniswap,
            pool,
        }))
    }

    pub async fn prepare<C: Network>(
        &self,
        node: &Node,
        trade: &Trade,
        fee: Option<AppFee>,
    ) -> Result<PreparedSwap> {
        trade.validate()?;
        let route = self.route::<C>(node, trade).await?;
        let quote = quote_trade::<C>(node, trade, route, fee).await?;
        let swap = router::Swap {
            router: C::UNISWAP.cswap_router,
            wallet: trade.wallet,
            route,
            amount_in: trade.amount,
            min_out: quote.min_out,
            fee,
            deadline: deadline()?,
        };
        Ok(PreparedSwap {
            transactions: swap.transactions::<C>(node).await?,
            quote,
            wallet: trade.wallet,
            created_at: std::time::Instant::now(),
        })
    }

    /// The trade's own pool, or the deepest one when it names none.
    async fn route<C: Network>(&self, node: &Node, trade: &Trade) -> Result<Route> {
        let (direction, token) = eth_pair(trade)?;
        let pool = match trade.pool {
            Some(pool) => pool,
            None => {
                self.find_market::<C>(node, token)
                    .await?
                    .ok_or_else(|| venue_error("no ETH pool for this token; use Relay"))?
                    .pool
            }
        };
        Ok(Route {
            pool,
            token,
            direction,
        })
    }
}

/// The pool, token and direction one trade swaps through.
#[derive(Clone, Copy)]
struct Route {
    pool: UniswapPool,
    token: Address,
    direction: Direction,
}

/// The router's fee is taken from the ETH side: off the input on buys, the output on sells.
async fn quote_trade<C: Network>(
    node: &Node,
    trade: &Trade,
    route: Route,
    fee: Option<AppFee>,
) -> Result<Quote> {
    let deployment = C::UNISWAP;
    let fee_in = match route.direction {
        Direction::Buy => fee_on(fee, trade.amount),
        Direction::Sell => Amount::ZERO,
    };
    let (pool_quote, usd_per_wei) = tokio::join!(
        quote(node, &deployment, route, trade.amount - fee_in),
        v3::usd_per_wei(node, &deployment)
    );
    let pool_quote = pool_quote?;
    let fee_out = match route.direction {
        Direction::Buy => Amount::ZERO,
        Direction::Sell => fee_on(fee, pool_quote.amount_out),
    };
    let expected_out = pool_quote.amount_out - fee_out;
    let min_out = trade.min_out(expected_out);
    if min_out.is_zero() {
        return Err(venue_error("quoted output is zero"));
    }
    let charged = fee_in + fee_out;
    Ok(Quote {
        chain_id: C::CHAIN_ID,
        source: VENUE,
        input: trade.input,
        output: trade.output,
        in_amount: trade.amount,
        expected_out,
        min_out,
        application_fee: (!charged.is_zero()).then_some(FeeAmount {
            currency: Currency::Native,
            amount: charged,
        }),
        usd_value: usd_per_wei.ok().and_then(|usd_per_wei| {
            usd_value(
                route.direction,
                trade.amount,
                expected_out,
                &pool_quote,
                usd_per_wei,
            )
        }),
    })
}

fn fee_on(fee: Option<AppFee>, amount: Amount) -> Amount {
    fee.map_or(Amount::ZERO, |fee| fee.of(amount))
}

/// Exactly one side must be ETH; the other is the token.
fn eth_pair(trade: &Trade) -> Result<(Direction, Address)> {
    match (trade.input, trade.output) {
        (Currency::Native, Currency::Token(token)) => Ok((Direction::Buy, token)),
        (Currency::Token(token), Currency::Native) => Ok((Direction::Sell, token)),
        _ => Err(TradeError::Build(
            "Uniswap trades ETH pairs only; use Relay for other pairs".into(),
        )),
    }
}

/// One exact-input swap to price against a pool.
#[derive(Clone, Copy)]
struct SwapIn {
    token: Address,
    direction: Direction,
    amount: Amount,
}

async fn quote(
    node: &Node,
    deployment: &UniswapDeployment,
    route: Route,
    amount_in: Amount,
) -> Result<PoolQuote> {
    let swap = SwapIn {
        token: route.token,
        direction: route.direction,
        amount: amount_in,
    };
    match route.pool {
        UniswapPool::V2 { pair } => v2::quote(node, deployment, pair, swap).await,
        UniswapPool::V3 { pool } => v3::quote(node, deployment, pool, swap).await,
        UniswapPool::V4 { key } => v4::quote(node, deployment, key, swap).await,
    }
}

/// Paid and received in USD from pool spot prices; `None` when a price is unusable.
fn usd_value(
    direction: Direction,
    amount_in: Amount,
    expected_out: Amount,
    pool: &PoolQuote,
    usd_per_wei: f64,
) -> Option<UsdValue> {
    let usd_per_token_unit = pool.eth_per_token * usd_per_wei;
    let (paid, received) = match direction {
        Direction::Buy => (
            to_f64(amount_in) * usd_per_wei,
            to_f64(expected_out) * usd_per_token_unit,
        ),
        Direction::Sell => (
            to_f64(amount_in) * usd_per_token_unit,
            to_f64(expected_out) * usd_per_wei,
        ),
    };
    UsdValue::new(paid, received)
}

/// token1 base units per token0 base unit, from a Q64.96 square-root price.
fn price_from_sqrt(sqrt_price_x96: Amount) -> f64 {
    let sqrt_price = to_f64(sqrt_price_x96) / Q96;
    sqrt_price * sqrt_price
}

/// Wei per token base unit, given which side of the pool ETH is on.
fn eth_per_token(token1_per_token0: f64, eth_is_token0: bool) -> f64 {
    if eth_is_token0 {
        1.0 / token1_per_token0
    } else {
        token1_per_token0
    }
}

/// USD display only; amounts stay exact integers.
fn to_f64(amount: Amount) -> f64 {
    f64::from(amount)
}

fn deadline() -> Result<Amount> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TradeError::Build("system clock is before 1970".into()))?;
    Ok(Amount::from((now + DEADLINE).as_secs()))
}

/// v2 pairs and v3 pools both expose their tokens; the factories decide which one it is.
async fn address_eth_pool(
    node: &Node,
    deployment: &UniswapDeployment,
    address: Address,
) -> Result<Option<EthPool>> {
    let (token0, token1) = tokio::join!(
        try_read(node, address, abi::IUniswapV2Pair::token0Call {}),
        try_read(node, address, abi::IUniswapV2Pair::token1Call {})
    );
    let (Some(token0), Some(token1)) = (token0?, token1?) else {
        return Ok(None);
    };
    let token = match (token0 == deployment.weth, token1 == deployment.weth) {
        (true, false) => token1,
        (false, true) => token0,
        _ => return Ok(None),
    };
    let pool = match v3::official_pool(node, deployment, address, token).await? {
        Some(pool) => Some(pool),
        None => v2::official_pair(node, deployment, address, token).await?,
    };
    Ok(pool.map(|pool| EthPool { token, pool }))
}

/// `None` when the call reverts or returns something else, i.e. `to` is not that contract.
async fn try_read<T: SolCall>(node: &Node, to: Address, call: T) -> Result<Option<T::Return>> {
    let Some(data) = node.try_call(to, call.abi_encode()).await? else {
        return Ok(None);
    };
    Ok(T::abi_decode_returns(&data).ok())
}

async fn read<T: SolCall>(node: &Node, to: Address, call: T) -> Result<T::Return> {
    let data = node.call(to, call.abi_encode()).await?;
    T::abi_decode_returns(&data)
        .map_err(|error| venue_error(format!("decode {}: {error}", T::SIGNATURE)))
}

/// Runs independent reads concurrently; results come back in completion order.
async fn in_parallel<T, F>(tasks: impl IntoIterator<Item = F>) -> Vec<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let mut set = JoinSet::new();
    for task in tasks {
        set.spawn(task);
    }
    let mut results = Vec::new();
    while let Some(result) = set.join_next().await {
        if let Ok(value) = result {
            results.push(value);
        }
    }
    results
}

fn venue_error(message: impl Into<String>) -> TradeError {
    TradeError::Venue {
        venue: VENUE,
        msg: message.into(),
    }
}
