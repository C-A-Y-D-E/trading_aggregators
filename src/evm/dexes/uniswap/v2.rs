use super::abi::{IUniswapV2Factory, IUniswapV2Pair};
use super::*;

/// Uniswap v2 keeps 0.3% of every input.
const INPUT_KEPT_PER_MILLE: u64 = 997;
const PER_MILLE: u64 = 1_000;

pub(super) async fn find(
    node: &Node,
    deployment: &UniswapDeployment,
    token: Address,
) -> Result<Option<UniswapPool>> {
    let pair = eth_pair(node, deployment, token).await?;
    Ok((!pair.is_zero()).then_some(UniswapPool::V2 { pair }))
}

/// `Some` when `pair` is the v2 factory's ETH pair for `token`.
pub(super) async fn official_pair(
    node: &Node,
    deployment: &UniswapDeployment,
    pair: Address,
    token: Address,
) -> Result<Option<UniswapPool>> {
    let official = eth_pair(node, deployment, token).await? == pair;
    Ok(official.then_some(UniswapPool::V2 { pair }))
}

pub(super) async fn quote(
    node: &Node,
    deployment: &UniswapDeployment,
    pair: Address,
    swap: SwapIn,
) -> Result<PoolQuote> {
    let (expected_pair, reserves) = tokio::join!(
        eth_pair(node, deployment, swap.token),
        reserves(node, pair, deployment.weth, swap.token)
    );
    if expected_pair? != pair {
        return Err(venue_error("pair is not the token's Uniswap v2 ETH pair"));
    }
    let (eth_reserve, token_reserve) = reserves?;
    let (reserve_in, reserve_out) = match swap.direction {
        Direction::Buy => (eth_reserve, token_reserve),
        Direction::Sell => (token_reserve, eth_reserve),
    };
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(venue_error("v2 pair has no liquidity"));
    }
    let in_after_fee = swap.amount * Amount::from(INPUT_KEPT_PER_MILLE);
    let amount_out =
        in_after_fee * reserve_out / (reserve_in * Amount::from(PER_MILLE) + in_after_fee);
    Ok(PoolQuote {
        amount_out,
        eth_per_token: to_f64(eth_reserve) / to_f64(token_reserve),
    })
}

async fn eth_pair(node: &Node, deployment: &UniswapDeployment, token: Address) -> Result<Address> {
    let call = IUniswapV2Factory::getPairCall {
        tokenA: token,
        tokenB: deployment.weth,
    };
    read(node, deployment.v2_factory, call).await
}

/// `(eth, token)` reserves; v2 pairs order their tokens by address.
async fn reserves(
    node: &Node,
    pair: Address,
    weth: Address,
    token: Address,
) -> Result<(Amount, Amount)> {
    let reserves = read(node, pair, IUniswapV2Pair::getReservesCall {}).await?;
    let reserve0 = Amount::from(reserves.reserve0.to::<u128>());
    let reserve1 = Amount::from(reserves.reserve1.to::<u128>());
    Ok(if weth < token {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    })
}
