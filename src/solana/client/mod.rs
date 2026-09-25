use std::sync::Arc;
use std::time::Duration;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_message::AddressLookupTableAccount;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use crate::error::{Result, TradeError};
use crate::solana::dexes::common::ata;
use crate::solana::dexes::pumpfun::PumpFun;
use crate::solana::dexes::pumpswap::PumpSwap;
use crate::solana::executor::{
    SwapSigners, check_status, confirm, dex_err, output_balance, submit_swap, token_balance,
};
use crate::solana::lookup_table::{load_address_lookup_tables, merge_lookup_tables};
use crate::solana::sdk_fee::SdkFee;
use crate::solana::sources::jupiter::Jupiter;
use crate::solana::types::{
    Dex, NativeMarket, PreparedSwap, Quote, QuoteSource, Settlement, Signer, Submitter, SwapResult,
    SwapStatus, Trade, Venue,
};
use crate::{Bloxroute, DFlow, GasSponsor, Relay};

mod routing;
mod usd_value;

pub struct TradingClient {
    rpc: Arc<RpcClient>,
    pumpfun: PumpFun,
    pumpswap: PumpSwap,
    jupiter: Jupiter,
    dflow: DFlow,
    bloxroute: Bloxroute,
    relay: Relay,
    quote_source: Option<QuoteSource>,
    sdk_fee: Option<SdkFee>,
    gas_sponsor: Option<GasSponsor>,
    usd_value_enabled: bool,
    shared_lookup_tables: Vec<AddressLookupTableAccount>,

    pub deadline: Duration,
}

impl TradingClient {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            pumpfun: PumpFun::new(rpc.clone()),
            pumpswap: PumpSwap::new(rpc.clone()),
            jupiter: Jupiter::new(),
            dflow: DFlow::new(rpc.clone()),
            bloxroute: Bloxroute::new(),
            relay: Relay::new(rpc.clone()),
            quote_source: None,
            sdk_fee: None,
            gas_sponsor: None,
            usd_value_enabled: true,
            shared_lookup_tables: Vec::new(),
            deadline: Duration::from_secs(30),
            rpc,
        }
    }

    /// The PumpSwap SOL/USDC pool that prices SOL for `quote.usd_value`.
    pub fn with_pumpswap_sol_usdc_pool(mut self, pool: Pubkey) -> Self {
        self.pumpswap = self.pumpswap.with_sol_usdc_pool(pool);
        self
    }

    pub fn with_sdk_fee(mut self, fee: SdkFee) -> Self {
        self.sdk_fee = Some(fee);
        self
    }

    /// Every USDC-settled trade on this client is sponsored, whatever the user's SOL balance;
    /// sources that can't be sponsored (bloXroute, and Pump.fun/PumpSwap, which trade SOL only)
    /// return an error. Use a separate client for unsponsored trades.
    pub fn with_gas_sponsor(mut self, sponsor: GasSponsor) -> Self {
        self.gas_sponsor = Some(sponsor);
        self
    }

    /// Skips USD pricing and its pool reads; `quote.usd_value` is then always `None`.
    pub fn without_usd_value(mut self) -> Self {
        self.usd_value_enabled = false;
        self
    }

    fn sponsor_for(&self, trade: &Trade) -> Option<&GasSponsor> {
        self.gas_sponsor
            .as_ref()
            .filter(|_| trade.settlement == Settlement::Usdc)
    }

    pub fn with_jupiter_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.jupiter = self.jupiter.with_api_key(api_key);
        self
    }

    pub fn with_jupiter_url(mut self, base_url: impl Into<String>) -> Self {
        self.jupiter = self.jupiter.with_base_url(base_url);
        self
    }

    pub fn with_dflow_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.dflow = self.dflow.with_api_key(api_key);
        self
    }

    pub fn with_dflow_url(mut self, base_url: impl Into<String>) -> Self {
        self.dflow = self.dflow.with_base_url(base_url);
        self
    }

    pub fn with_bloxroute_auth_header(mut self, auth_header: impl Into<String>) -> Self {
        self.bloxroute = self.bloxroute.with_auth_header(auth_header);
        self
    }

    pub fn with_bloxroute_url(mut self, base_url: impl Into<String>) -> Self {
        self.bloxroute = self.bloxroute.with_base_url(base_url);
        self
    }

    pub fn with_relay_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.relay = self.relay.with_api_key(api_key);
        self
    }

    pub fn with_relay_url(mut self, base_url: impl Into<String>) -> Self {
        self.relay = self.relay.with_base_url(base_url);
        self
    }

    pub fn with_quote_source(mut self, source: QuoteSource) -> Self {
        self.quote_source = Some(source);
        self
    }

    pub fn with_shared_lookup_tables(mut self, tables: Vec<AddressLookupTableAccount>) -> Self {
        self.shared_lookup_tables = merge_lookup_tables(&tables, &[]);
        self
    }

    pub async fn with_shared_lookup_table_addresses(
        mut self,
        addresses: &[Pubkey],
    ) -> Result<Self> {
        self.shared_lookup_tables = load_address_lookup_tables(&self.rpc, addresses).await?;
        Ok(self)
    }

    pub fn shared_lookup_tables(&self) -> &[AddressLookupTableAccount] {
        &self.shared_lookup_tables
    }

    /// Call after extending a table. A failed refresh leaves the old snapshots in place.
    pub async fn refresh_shared_lookup_tables(&mut self) -> Result<()> {
        let addresses: Vec<_> = self
            .shared_lookup_tables
            .iter()
            .map(|table| table.key)
            .collect();
        let refreshed = load_address_lookup_tables(&self.rpc, &addresses).await?;
        self.shared_lookup_tables = refreshed;
        Ok(())
    }

    /// `None` means the token has no tradable Pump.fun or PumpSwap market; use an aggregator.
    pub async fn find_native_market(&self, mint: &Pubkey) -> Result<Option<NativeMarket>> {
        let (bonding_curve, graduated_pool) = tokio::join!(
            self.pumpfun.find_market(mint),
            self.pumpswap.find_market(mint)
        );
        let bonding_curve = bonding_curve.map_err(|error| dex_err(self.pumpfun.name(), error))?;
        let graduated_pool =
            graduated_pool.map_err(|error| dex_err(self.pumpswap.name(), error))?;
        // A live curve means the token has not graduated, so it takes priority.
        Ok(bonding_curve.or(graduated_pool))
    }

    pub async fn quote(&self, trade: &Trade) -> Result<Quote> {
        Ok(self.prepare_swap(trade).await?.quote)
    }

    pub async fn prepare_swap(&self, trade: &Trade) -> Result<PreparedSwap> {
        self.prepare_selected_swap(trade).await
    }

    pub async fn swap(
        &self,
        trade: &Trade,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
        priority_fee_lamports: u64,
    ) -> Result<SwapResult> {
        self.swap_with_lookup_tables(trade, signer, submitter, priority_fee_lamports, &[])
            .await
    }

    pub async fn swap_with_lookup_tables(
        &self,
        trade: &Trade,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
        priority_fee_lamports: u64,
        lookup_tables: &[AddressLookupTableAccount],
    ) -> Result<SwapResult> {
        let before = output_balance(&self.rpc, trade).await;

        let pending = self
            .submit_with_lookup_tables(
                trade,
                signer,
                submitter,
                priority_fee_lamports,
                lookup_tables,
            )
            .await?;
        let sig = pending
            .hash
            .parse::<Signature>()
            .map_err(|_| TradeError::Decode("client", format!("bad signature {}", pending.hash)))?;

        let status = confirm(&self.rpc, &sig, self.deadline).await?;
        let amount_received = match status {
            SwapStatus::Confirmed => {
                let after = output_balance(&self.rpc, trade).await;
                before
                    .zip(after)
                    .map(|(before, after)| after.saturating_sub(before))
            }
            SwapStatus::Failed | SwapStatus::Pending => None,
        };
        Ok(SwapResult {
            hash: pending.hash,
            dex: pending.dex,
            status,
            amount_received,
            application_fee: pending.application_fee,
            sponsorship_fee: pending.sponsorship_fee,
        })
    }

    pub async fn submit(
        &self,
        trade: &Trade,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
        priority_fee_lamports: u64,
    ) -> Result<SwapResult> {
        self.submit_with_lookup_tables(trade, signer, submitter, priority_fee_lamports, &[])
            .await
    }

    pub async fn submit_with_lookup_tables(
        &self,
        trade: &Trade,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
        priority_fee_lamports: u64,
        lookup_tables: &[AddressLookupTableAccount],
    ) -> Result<SwapResult> {
        let combined_tables = merge_lookup_tables(lookup_tables, &self.shared_lookup_tables);
        let prepared = self.prepare_swap(trade).await?;
        // No fallback after preparation: a submission error may mean the transaction landed.
        submit_swap(
            &self.rpc,
            prepared,
            SwapSigners {
                user: signer,
                sponsor: self.sponsor_for(trade),
            },
            submitter,
            trade,
            priority_fee_lamports,
            &combined_tables,
        )
        .await
    }

    pub async fn status(&self, hash: &str) -> Result<SwapStatus> {
        let sig = hash
            .parse::<Signature>()
            .map_err(|_| TradeError::Decode("client", format!("bad signature {hash:?}")))?;
        check_status(&self.rpc, &sig).await
    }

    /// Returns zero when the wallet has no token account for this mint.
    pub async fn token_balance(&self, wallet: &Pubkey, mint: &Pubkey) -> Result<u64> {
        let mint_account = self
            .rpc
            .get_account(mint)
            .await
            .map_err(|source| TradeError::Rpc {
                context: "get_account(mint)",
                source,
            })?;
        token_balance(&self.rpc, &ata(wallet, mint, &mint_account.owner)).await
    }
}
