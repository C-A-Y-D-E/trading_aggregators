use alloy_primitives::aliases::{U24, U160};

use super::abi::{IQuoterV2, IUniswapV3Factory, IUniswapV3Pool};
use super::*;

const FEE_TIERS: [u32; 4] = [100, 500, 3_000, 10_000];

pub(super) async fn find(
    node: &Node,
    deployment: &UniswapDeployment,
    token: Address,
) -> Result<Vec<UniswapPool>> {
    let tasks = FEE_TIERS.map(|fee| {
        let (node, deployment) = (node.clone(), *deployment);
        async move { eth_pool(&node, &deployment, token, U24::from(fee)).await }
    });
    let pools = in_parallel(tasks)
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    Ok(pools
        .into_iter()
        .filter(|pool| !pool.is_zero())
        .map(|pool| UniswapPool::V3 { pool })
        .collect())
}

/// `Some` when `pool` is the v3 factory's ETH pool for `token` at its fee tier. v2 pairs have
/// no `fee()`, so they read as `None` here.
pub(super) async fn official_pool(
    node: &Node,
    deployment: &UniswapDeployment,
    pool: Address,
    token: Address,
) -> Result<Option<UniswapPool>> {
    let Some(fee) = try_read(node, pool, IUniswapV3Pool::feeCall {}).await? else {
        return Ok(None);
    };
    let official = eth_pool(node, deployment, token, fee).await? == pool;
    Ok(official.then_some(UniswapPool::V3 { pool }))
}

pub(super) async fn quote(
    node: &Node,
    deployment: &UniswapDeployment,
    pool: Address,
    swap: SwapIn,
) -> Result<PoolQuote> {
    let fee = read(node, pool, IUniswapV3Pool::feeCall {}).await?;
    let (token_in, token_out) = match swap.direction {
        Direction::Buy => (deployment.weth, swap.token),
        Direction::Sell => (swap.token, deployment.weth),
    };
    let params = IQuoterV2::QuoteExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        amountIn: swap.amount,
        fee,
        sqrtPriceLimitX96: U160::ZERO,
    };
    let (expected_pool, slot0, quoted) = tokio::join!(
        eth_pool(node, deployment, swap.token, fee),
        read(node, pool, IUniswapV3Pool::slot0Call {}),
        read(
            node,
            deployment.v3_quoter,
            IQuoterV2::quoteExactInputSingleCall { params }
        )
    );
    if expected_pool? != pool {
        return Err(venue_error(
            "pool is not a Uniswap v3 ETH pool for this token",
        ));
    }
    let price = price_from_sqrt(Amount::from(slot0?.sqrtPriceX96));
    Ok(PoolQuote {
        amount_out: quoted?.amountOut,
        eth_per_token: eth_per_token(price, deployment.weth < swap.token),
    })
}

/// USD per wei, from the deployment's WETH/stablecoin pool.
pub(super) async fn usd_per_wei(node: &Node, deployment: &UniswapDeployment) -> Result<f64> {
    let pool = deployment.eth_usd_pool;
    let (token0, slot0) = tokio::join!(
        read(node, pool, IUniswapV3Pool::token0Call {}),
        read(node, pool, IUniswapV3Pool::slot0Call {})
    );
    let price = price_from_sqrt(Amount::from(slot0?.sqrtPriceX96));
    let usd_units_per_wei = eth_per_token(price, token0? == deployment.weth).recip();
    Ok(usd_units_per_wei / 10f64.powi(deployment.usd_decimals.into()))
}

async fn eth_pool(
    node: &Node,
    deployment: &UniswapDeployment,
    token: Address,
    fee: U24,
) -> Result<Address> {
    let call = IUniswapV3Factory::getPoolCall {
        tokenA: token,
        tokenB: deployment.weth,
        fee,
    };
    read(node, deployment.v3_factory, call).await
}
