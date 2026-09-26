use super::super::super::Transaction;
use super::abi::{ICswapRouter, IERC20, PoolKey as AbiPoolKey};
use super::*;

/// One router swap; `amount_in` is the full trade amount, fee included.
pub(super) struct Swap {
    pub router: Address,
    pub wallet: Address,
    pub route: Route,
    pub amount_in: Amount,
    pub min_out: Amount,
    pub fee: Option<AppFee>,
    pub deadline: Amount,
}

impl Swap {
    pub async fn transactions<C: Network>(&self, node: &Node) -> Result<Vec<Transaction>> {
        match self.route.direction {
            Direction::Buy => Ok(vec![self.buy::<C>()?]),
            Direction::Sell => self.sell_with_approval::<C>(node).await,
        }
    }

    fn buy<C: Network>(&self) -> Result<Transaction> {
        let call = ICswapRouter::buyCall {
            pool: self.router_pool()?,
            token: self.route.token,
            minOut: self.min_out,
            feeBps: self.fee_bps(),
            deadline: self.deadline,
        };
        Ok(self.transaction::<C>(self.router, call.abi_encode(), self.amount_in))
    }

    /// Approves exactly this amount first when the router's allowance is short.
    async fn sell_with_approval<C: Network>(&self, node: &Node) -> Result<Vec<Transaction>> {
        let token = self.route.token;
        let allowance = IERC20::allowanceCall {
            owner: self.wallet,
            spender: self.router,
        };
        let mut transactions = Vec::new();
        if read(node, token, allowance).await? < self.amount_in {
            let approve = IERC20::approveCall {
                spender: self.router,
                amount: self.amount_in,
            };
            transactions.push(self.transaction::<C>(token, approve.abi_encode(), Amount::ZERO));
        }
        let sell = ICswapRouter::sellCall {
            pool: self.router_pool()?,
            token,
            amountIn: self.amount_in,
            minOut: self.min_out,
            feeBps: self.fee_bps(),
            deadline: self.deadline,
        };
        transactions.push(self.transaction::<C>(self.router, sell.abi_encode(), Amount::ZERO));
        Ok(transactions)
    }

    fn router_pool(&self) -> Result<ICswapRouter::Pool> {
        let (version, pool, v4_key) = match self.route.pool {
            UniswapPool::V2 { pair } => (ICswapRouter::Version::V2, pair, empty_key()),
            UniswapPool::V3 { pool } => (ICswapRouter::Version::V3, pool, empty_key()),
            UniswapPool::V4 { key } => (ICswapRouter::Version::V4, Address::ZERO, key.to_abi()?),
        };
        Ok(ICswapRouter::Pool {
            version,
            pool,
            v4Key: v4_key,
        })
    }

    /// The router pays its own stored fee wallet, so only the rate travels with the swap.
    fn fee_bps(&self) -> u16 {
        self.fee.map_or(0, |fee| fee.basis_points())
    }

    fn transaction<C: Network>(&self, to: Address, data: Vec<u8>, value: Amount) -> Transaction {
        Transaction {
            chain_id: C::CHAIN_ID,
            from: self.wallet,
            to,
            data: data.into(),
            value,
            gas: None,
            gas_price: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
        }
    }
}

fn empty_key() -> AbiPoolKey {
    AbiPoolKey {
        currency0: Address::ZERO,
        currency1: Address::ZERO,
        fee: Default::default(),
        tickSpacing: Default::default(),
        hooks: Address::ZERO,
    }
}
