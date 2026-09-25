use async_trait::async_trait;
use solana_instruction::Instruction;
use solana_message::AddressLookupTableAccount;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::error::{Result, TradeError};
use crate::solana::dexes::common::WSOL;
use crate::{USDC_MINT, UsdValue};

pub(crate) const BASIS_POINTS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Venue {
    PumpFun,
    PumpSwap,
}

/// Exactly one quote/execution source. Failures never switch to another source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteSource {
    Jupiter,
    DFlow,
    Bloxroute,
    Relay,
    PumpFun,
    PumpSwap,
}

impl QuoteSource {
    pub(crate) fn native_venue(self) -> Option<Venue> {
        match self {
            Self::PumpFun => Some(Venue::PumpFun),
            Self::PumpSwap => Some(Venue::PumpSwap),
            Self::Jupiter | Self::DFlow | Self::Bloxroute | Self::Relay => None,
        }
    }
}

/// A SOL-paired Pump.fun curve or PumpSwap pool. Pass `source` and `pool` to a SOL-settled
/// `Trade`; USDC trades go through an aggregator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeMarket {
    pub source: QuoteSource,
    pub pool: Pubkey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Sol,
    Usdc,
}

impl Settlement {
    pub fn mint(self) -> Pubkey {
        match self {
            Self::Sol => WSOL,
            Self::Usdc => USDC_MINT,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Trade {
    pub settlement: Settlement,
    pub wallet: Pubkey,

    pub mint: Pubkey,
    pub side: Side,

    pub amount: u64,

    pub slippage_bps: u64,

    /// Legacy native selection, used only when neither trade nor client specifies a quote source.
    pub venue: Option<Venue>,

    pub pool: Option<Pubkey>,

    pub quote_source: Option<QuoteSource>,
}

impl Trade {
    pub fn buy(
        wallet: Pubkey,
        mint: Pubkey,
        amount: u64,
        slippage_bps: u64,
        venue: Option<Venue>,
    ) -> Self {
        Self {
            settlement: Settlement::Sol,
            wallet,
            mint,
            side: Side::Buy,
            amount,
            slippage_bps,
            venue,
            pool: None,
            quote_source: None,
        }
    }

    pub fn sell(
        wallet: Pubkey,
        mint: Pubkey,
        amount: u64,
        slippage_bps: u64,
        venue: Option<Venue>,
    ) -> Self {
        Self {
            settlement: Settlement::Sol,
            wallet,
            mint,
            side: Side::Sell,
            amount,
            slippage_bps,
            venue,
            pool: None,
            quote_source: None,
        }
    }

    pub fn with_pool(mut self, pool: Pubkey) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Selects buy funding or sell proceeds: SOL lamports or USDC base units (6 decimals).
    pub fn with_settlement(mut self, settlement: Settlement) -> Self {
        self.settlement = settlement;
        self
    }

    pub fn with_quote_source(mut self, source: QuoteSource) -> Self {
        self.quote_source = Some(source);
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.amount == 0
            || self.slippage_bps >= BASIS_POINTS
            || self.mint == self.settlement.mint()
        {
            return Err(TradeError::Build(
                "trade requires a positive amount, slippage below 10000 bps and a token other than the settlement currency".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Quote {
    pub in_amount: u64,

    /// From pool spot prices when the Pump pool is known, else the source's USD amounts.
    /// `None` when neither is available or the client disables it.
    // TODO: price Jupiter and DFlow trades that carry no Pump pool (e.g. via Jupiter's price API).
    pub usd_value: Option<UsdValue>,

    pub expected_out: u64,

    pub min_out: u64,

    /// Informational. Aggregators report zero when they don't itemize fees, not when trading is free.
    pub fee: u64,
    /// In settlement units; outputs are already net of it. Provider-collected sell fees can vary
    /// at execution.
    pub application_fee: u64,
    /// The reserved USDC ceiling; the final charge is set before signing.
    pub sponsorship_fee: u64,
}

#[derive(Debug)]
pub struct PreparedSwap {
    pub venue: &'static str,
    pub quote: Quote,
    pub instructions: Vec<Instruction>,
    pub lookup_tables: Vec<AddressLookupTableAccount>,
}

#[derive(Debug, Clone, Copy)]
pub struct Tip {
    pub account: Pubkey,
    pub lamports: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapStatus {
    Confirmed,
    Failed,

    Pending,
}

#[derive(Debug, Clone)]
pub struct SwapResult {
    pub hash: String,

    pub dex: &'static str,
    pub status: SwapStatus,

    pub amount_received: Option<u64>,

    pub application_fee: u64,

    /// Collected only if the transaction succeeds on-chain.
    pub sponsorship_fee: u64,
}

#[async_trait]
pub trait Dex: Send + Sync {
    fn name(&self) -> &'static str;
    async fn quote(&self, trade: &Trade) -> anyhow::Result<Quote>;

    async fn swap(
        &self,
        trade: &Trade,
    ) -> anyhow::Result<(Vec<Instruction>, Vec<AddressLookupTableAccount>)> {
        let prepared = self.prepare_swap(trade).await?;
        Ok((prepared.instructions, prepared.lookup_tables))
    }

    /// Must return the quote enforced by these instructions, without independently requoting.
    async fn prepare_swap(&self, trade: &Trade) -> anyhow::Result<PreparedSwap>;

    async fn prepare_sponsored_swap(
        &self,
        _trade: &Trade,
        _payer: &Pubkey,
    ) -> anyhow::Result<PreparedSwap> {
        anyhow::bail!("{} does not support sponsored swaps", self.name())
    }
}

#[async_trait]
pub trait Signer: Send + Sync {
    async fn sign(&self, wallet: &Pubkey, tx: &VersionedTransaction) -> anyhow::Result<Signature>;
}

#[async_trait]
pub trait Submitter: Send + Sync {
    async fn submit(&self, tx: &VersionedTransaction) -> anyhow::Result<Signature>;

    fn default_tip(&self) -> Option<Tip> {
        None
    }
}
