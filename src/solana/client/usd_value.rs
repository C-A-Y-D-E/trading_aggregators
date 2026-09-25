use anyhow::{anyhow, ensure};

use super::*;
use crate::solana::dexes::common::WSOL;
use crate::solana::dexes::pumpswap::PoolVaults;
use crate::solana::dexes::{pumpfun, pumpswap};
use crate::solana::executor::token_account_amount;
use crate::{Side, USDC_MINT, UsdValue};

const USDC_UNITS_PER_DOLLAR: f64 = 1_000_000.0;

/// USD per base unit of the traded token and of the settlement currency, at pool spot prices.
pub(super) struct PoolPrices {
    token: f64,
    settlement: f64,
}

impl PoolPrices {
    pub(super) fn usd_value(&self, trade: &Trade, quote: &Quote) -> Option<UsdValue> {
        let (paid_price, received_price) = match trade.side {
            Side::Buy => (self.settlement, self.token),
            Side::Sell => (self.token, self.settlement),
        };
        UsdValue::new(
            quote.in_amount as f64 * paid_price,
            quote.expected_out as f64 * received_price,
        )
    }
}

impl TradingClient {
    /// USD is display-only: a failed pool read gives `None`, and the provider's values are used.
    pub(super) async fn pool_prices(
        &self,
        trade: &Trade,
        source: QuoteSource,
    ) -> Option<PoolPrices> {
        if !self.usd_value_enabled {
            return None;
        }
        let pool = trade.pool.or_else(|| default_pool(trade, source))?;
        self.read_pool_prices(trade, pool).await.ok()
    }

    async fn read_pool_prices(&self, trade: &Trade, pool: Pubkey) -> anyhow::Result<PoolPrices> {
        let [sol_usdc_pool, usdc_vault, sol_vault] =
            self.pumpswap.sol_usdc_price_accounts().await?;
        let accounts = self
            .rpc
            .get_multiple_accounts(&[pool, sol_usdc_pool, usdc_vault, sol_vault])
            .await?;
        let [pool_account, sol_usdc_pool, usdc_vault, sol_vault] = all_present(accounts)?;
        let usd_per_lamport =
            usd_per_lamport(&sol_usdc_pool.data, &usdc_vault.data, &sol_vault.data)?;
        let (quote_mint, token_spot) = match pool_account.owner {
            pumpfun::PROGRAM_ID => (
                WSOL,
                pumpfun::curve_spot_price(&pool, &trade.mint, &pool_account.data)?,
            ),
            pumpswap::PROGRAM_ID => self.pumpswap_spot(&trade.mint, &pool_account.data).await?,
            owner => return Err(anyhow!("no spot price for pools owned by {owner}")),
        };
        Ok(PoolPrices {
            token: token_spot * usd_per_unit(quote_mint, usd_per_lamport)?,
            settlement: usd_per_unit(trade.settlement.mint(), usd_per_lamport)?,
        })
    }

    /// A PumpSwap pool keeps its reserves in vaults, so they need a second read.
    async fn pumpswap_spot(
        &self,
        mint: &Pubkey,
        pool_data: &[u8],
    ) -> anyhow::Result<(Pubkey, f64)> {
        let pool = PoolVaults::decode(pool_data, mint)?;
        let vaults = self.rpc.get_multiple_accounts(&pool.addresses()).await?;
        let [base_vault, quote_vault] = all_present(vaults)?;
        let spot = pool.spot_price(
            token_account_amount(&base_vault.data)?,
            token_account_amount(&quote_vault.data)?,
        )?;
        Ok((pool.quote_mint, spot))
    }
}

/// Providers price the routed amounts, so scale to what the user pays and gets after SDK
/// and sponsorship fees; otherwise the loss would hide our own fees.
pub(super) fn usd_value_after_fees(routed: &Quote, charged: &Quote) -> Option<UsdValue> {
    let reported = routed.usd_value?;
    UsdValue::new(
        reported.paid() * charged.in_amount as f64 / routed.in_amount as f64,
        reported.received() * charged.expected_out as f64 / routed.expected_out as f64,
    )
}

fn default_pool(trade: &Trade, source: QuoteSource) -> Option<Pubkey> {
    match source {
        QuoteSource::PumpFun => Some(PumpFun::bonding_curve_pda(&trade.mint)),
        QuoteSource::PumpSwap => Some(PumpSwap::canonical_pool_pda(&trade.mint)),
        _ => None,
    }
}

/// The SOL/USDC pool's base is USDC and its quote is SOL.
fn usd_per_lamport(
    pool_data: &[u8],
    usdc_vault_data: &[u8],
    sol_vault_data: &[u8],
) -> anyhow::Result<f64> {
    let pool = PoolVaults::decode(pool_data, &USDC_MINT)?;
    ensure!(
        pool.quote_mint == WSOL,
        "SOL/USDC pool must be quoted in SOL"
    );
    let lamports_per_usdc_unit = pool.spot_price(
        token_account_amount(usdc_vault_data)?,
        token_account_amount(sol_vault_data)?,
    )?;
    Ok(1.0 / (lamports_per_usdc_unit * USDC_UNITS_PER_DOLLAR))
}

fn usd_per_unit(mint: Pubkey, usd_per_lamport: f64) -> anyhow::Result<f64> {
    match mint {
        WSOL => Ok(usd_per_lamport),
        USDC_MINT => Ok(1.0 / USDC_UNITS_PER_DOLLAR),
        other => Err(anyhow!("no USD price for {other}")),
    }
}

fn all_present<T, const N: usize>(accounts: Vec<Option<T>>) -> anyhow::Result<[T; N]> {
    let accounts: Vec<T> = accounts
        .into_iter()
        .collect::<Option<_>>()
        .ok_or_else(|| anyhow!("pricing account not found"))?;
    accounts
        .try_into()
        .map_err(|_| anyhow!("unexpected pricing account count"))
}
