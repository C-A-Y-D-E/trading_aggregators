use alloy_primitives::aliases::{I24, U24};
use alloy_primitives::{B256, Bytes, FixedBytes, keccak256};
use alloy_sol_types::SolValue;

use super::abi::{self, IStateView, IV4Quoter};
use super::*;

/// Fee (hundredths of a bip) and tick spacing of the standard hookless tiers.
const STANDARD_TIERS: [(u32, i32); 4] = [(100, 1), (500, 10), (3_000, 60), (10_000, 200)];
const POSITION_MANAGER_ID_LEN: usize = 25;

pub(super) async fn find(
    node: &Node,
    deployment: &UniswapDeployment,
    token: Address,
) -> Result<Vec<UniswapPool>> {
    let keys = [Address::ZERO, deployment.weth]
        .into_iter()
        .flat_map(|eth| {
            STANDARD_TIERS.map(|(fee, tick_spacing)| hookless_key(eth, token, fee, tick_spacing))
        });
    let tasks = keys.map(|key| {
        let (node, deployment) = (node.clone(), *deployment);
        async move { initialized(&node, &deployment, key).await }
    });
    let found = in_parallel(tasks)
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    Ok(found.into_iter().flatten().collect())
}

/// The PositionManager records the key of every pool it has minted into, so pools that only
/// ever got liquidity through other contracts resolve to `None`.
pub(super) async fn eth_pool(
    node: &Node,
    deployment: &UniswapDeployment,
    id: B256,
) -> Result<Option<EthPool>> {
    let call = abi::IPositionManager::poolKeysCall {
        poolId: FixedBytes::from_slice(&id[..POSITION_MANAGER_ID_LEN]),
    };
    let Some(key) = try_read(node, deployment.v4_position_manager, call).await? else {
        return Ok(None);
    };
    let key = PoolKey::from_abi(key);
    // An unknown id reads back as an all-zero key, which fails the id check.
    let routable = key.hooks.is_zero() && key.id()? == id;
    let Some(eth) = key.eth_side(deployment.weth).filter(|_| routable) else {
        return Ok(None);
    };
    let pool = initialized(node, deployment, key).await?;
    Ok(pool.map(|pool| EthPool {
        token: eth.token,
        pool,
    }))
}

async fn initialized(
    node: &Node,
    deployment: &UniswapDeployment,
    key: PoolKey,
) -> Result<Option<UniswapPool>> {
    let slot0 = IStateView::getSlot0Call { poolId: key.id()? };
    let sqrt_price = read(node, deployment.v4_state_view, slot0)
        .await?
        .sqrtPriceX96;
    // An uninitialized pool reads as all zeros.
    Ok((!sqrt_price.is_zero()).then_some(UniswapPool::V4 { key }))
}

pub(super) async fn quote(
    node: &Node,
    deployment: &UniswapDeployment,
    key: PoolKey,
    swap: SwapIn,
) -> Result<PoolQuote> {
    let eth = key
        .eth_side(deployment.weth)
        .filter(|side| side.token == swap.token && key.hooks.is_zero())
        .ok_or_else(|| venue_error("v4 key must be a hookless ETH pool for this token"))?;
    let params = IV4Quoter::QuoteExactSingleParams {
        poolKey: key.to_abi()?,
        zeroForOne: (swap.direction == Direction::Buy) == eth.is_currency0,
        exactAmount: swap
            .amount
            .try_into()
            .map_err(|_| venue_error("amount too large for a v4 quote"))?,
        hookData: Bytes::new(),
    };
    let (slot0, quoted) = tokio::join!(
        read(
            node,
            deployment.v4_state_view,
            IStateView::getSlot0Call { poolId: key.id()? }
        ),
        read(
            node,
            deployment.v4_quoter,
            IV4Quoter::quoteExactInputSingleCall { params }
        )
    );
    let price = price_from_sqrt(Amount::from(slot0?.sqrtPriceX96));
    Ok(PoolQuote {
        amount_out: quoted?.amountOut,
        eth_per_token: eth_per_token(price, eth.is_currency0),
    })
}

/// Which currency is ETH (native zero address or WETH) and which is the token.
struct EthSide {
    is_currency0: bool,
    token: Address,
}

impl PoolKey {
    fn eth_side(&self, weth: Address) -> Option<EthSide> {
        let is_eth = |currency: Address| currency.is_zero() || currency == weth;
        match (is_eth(self.currency0), is_eth(self.currency1)) {
            (true, false) => Some(EthSide {
                is_currency0: true,
                token: self.currency1,
            }),
            (false, true) => Some(EthSide {
                is_currency0: false,
                token: self.currency0,
            }),
            _ => None,
        }
    }

    fn from_abi(key: abi::PoolKey) -> Self {
        Self {
            currency0: key.currency0,
            currency1: key.currency1,
            fee: key.fee.to(),
            tick_spacing: key.tickSpacing.as_i32(),
            hooks: key.hooks,
        }
    }

    /// `keccak256(abi.encode(key))`, the id v4 stores pool state under.
    fn id(&self) -> Result<B256> {
        Ok(keccak256(self.to_abi()?.abi_encode()))
    }

    pub(super) fn to_abi(self) -> Result<abi::PoolKey> {
        Ok(abi::PoolKey {
            currency0: self.currency0,
            currency1: self.currency1,
            fee: U24::try_from(self.fee).map_err(|_| venue_error("v4 fee exceeds 24 bits"))?,
            tickSpacing: I24::try_from(self.tick_spacing)
                .map_err(|_| venue_error("v4 tick spacing exceeds 24 bits"))?,
            hooks: self.hooks,
        })
    }
}

/// v4 orders currencies by address; the native zero address always comes first.
fn hookless_key(eth: Address, token: Address, fee: u32, tick_spacing: i32) -> PoolKey {
    let (currency0, currency1) = if eth < token {
        (eth, token)
    } else {
        (token, eth)
    };
    PoolKey {
        currency0,
        currency1,
        fee,
        tick_spacing,
        hooks: Address::ZERO,
    }
}
