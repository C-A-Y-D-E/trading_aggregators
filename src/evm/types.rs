use std::time::Instant;

use alloy_primitives::Bytes;
use async_trait::async_trait;
use serde::Serialize;

use super::{Address, Amount, TransactionHash, UniswapPool, UnsignedTransaction};
use crate::{Result, TradeError, UsdValue};

const BASIS_POINTS: u16 = 10_000;

/// Exactly one quote/execution source. Failures never switch to another source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteSource {
    Relay,
    /// ETH pairs only, one pool, through your `CswapRouter`.
    Uniswap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Currency {
    Native,
    Token(Address),
}

impl Currency {
    pub fn address(self) -> Address {
        match self {
            Self::Native => Address::ZERO,
            Self::Token(address) => address,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Trade {
    pub wallet: Address,
    pub input: Currency,
    pub output: Currency,
    pub amount: Amount,
    pub slippage_bps: u16,
    /// Overrides the client's source for this trade.
    pub quote_source: Option<QuoteSource>,
    /// Uniswap only: trade exactly this pool instead of the deepest ETH pool.
    pub pool: Option<UniswapPool>,
}

impl Trade {
    pub fn exact_input(
        wallet: Address,
        input: Currency,
        output: Currency,
        amount: Amount,
        slippage_bps: u16,
    ) -> Self {
        Self {
            wallet,
            input,
            output,
            amount,
            slippage_bps,
            quote_source: None,
            pool: None,
        }
    }

    pub fn with_quote_source(mut self, source: QuoteSource) -> Self {
        self.quote_source = Some(source);
        self
    }

    pub fn with_pool(mut self, pool: UniswapPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// The least output slippage allows.
    pub(super) fn min_out(&self, expected: Amount) -> Amount {
        portion(expected, BASIS_POINTS - self.slippage_bps)
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.wallet.is_zero()
            || self.amount.is_zero()
            || self.slippage_bps >= BASIS_POINTS
            || self.input.address() == self.output.address()
            || matches!(self.input, Currency::Token(address) if address.is_zero())
            || matches!(self.output, Currency::Token(address) if address.is_zero())
        {
            return Err(TradeError::Build("swap requires a wallet, distinct currencies, positive input and slippage below 10000 bps".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AppFee {
    recipient: Address,
    basis_points: u16,
}

impl AppFee {
    pub fn new(recipient: Address, basis_points: u16) -> Result<Self> {
        if recipient.is_zero() || basis_points >= 10_000 {
            return Err(TradeError::Build(
                "app fee requires a nonzero claim address and fee below 10000 bps".into(),
            ));
        }
        Ok(Self {
            recipient,
            basis_points,
        })
    }

    pub fn recipient(&self) -> Address {
        self.recipient
    }
    pub fn basis_points(&self) -> u16 {
        self.basis_points
    }

    /// The fee on `amount`, rounded down.
    pub(super) fn of(&self, amount: Amount) -> Amount {
        portion(amount, self.basis_points)
    }
}

/// `amount × bps / 10_000`, rounded down without overflowing on large amounts.
pub(super) fn portion(amount: Amount, bps: u16) -> Amount {
    let denominator = Amount::from(BASIS_POINTS);
    let numerator = Amount::from(bps);
    (amount / denominator) * numerator + (amount % denominator) * numerator / denominator
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeAmount {
    pub currency: Currency,
    pub amount: Amount,
}

#[derive(Debug, Clone)]
pub struct Quote {
    pub chain_id: u64,
    pub source: &'static str,
    pub input: Currency,
    pub output: Currency,
    pub in_amount: Amount,
    pub expected_out: Amount,
    pub min_out: Amount,
    /// Quoted on-chain charge; Relay credits the claim address's offchain USDC balance.
    pub application_fee: Option<FeeAmount>,
    /// `None` when the source reports no USD amounts.
    pub usd_value: Option<UsdValue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub chain_id: u64,
    pub from: Address,
    pub to: Address,
    pub data: alloy_primitives::Bytes,
    pub value: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas: Option<Amount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_price: Option<Amount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fee_per_gas: Option<Amount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_priority_fee_per_gas: Option<Amount>,
}

#[derive(Debug)]
pub struct PreparedSwap {
    pub(super) quote: Quote,
    pub(super) wallet: Address,
    pub(super) transactions: Vec<Transaction>,
    pub(super) created_at: Instant,
}

impl PreparedSwap {
    pub fn quote(&self) -> &Quote {
        &self.quote
    }
    pub fn transactions(&self) -> &[Transaction] {
        &self.transactions
    }
}

#[derive(Debug)]
pub struct SwapResult {
    pub quote: Quote,
    /// Successful approval transactions followed by the successful swap transaction.
    pub transaction_hashes: Vec<TransactionHash>,
}

#[derive(Debug, thiserror::Error)]
#[error("{source}; already submitted transactions: {submitted:?}")]
pub struct SwapError {
    #[source]
    pub source: TradeError,
    /// Every transaction handed to the submitter, including one whose submit call failed,
    /// since it may still land. Check these before retrying; nothing is retried automatically.
    pub submitted: Vec<TransactionHash>,
}

impl SwapError {
    pub(super) fn before_submission(source: TradeError) -> Self {
        Self {
            source,
            submitted: Vec::new(),
        }
    }
}

#[async_trait]
pub trait Signer: Send + Sync {
    /// Return EIP-2718 signed bytes for exactly this transaction; any change is rejected.
    async fn sign(
        &self,
        wallet: &Address,
        transaction: &UnsignedTransaction,
    ) -> anyhow::Result<Bytes>;
}

#[async_trait]
pub trait Submitter: Send + Sync {
    async fn submit(&self, signed: &Bytes) -> anyhow::Result<TransactionHash>;
}
