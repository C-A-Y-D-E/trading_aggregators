use std::future::Future;
use std::time::Duration;

use super::usd_value::usd_value_after_fees;
use super::*;
use crate::FeeCollection;
use crate::solana::provider_fee::ProviderFee;

const ROUTE_PREPARATION_TIMEOUT: Duration = Duration::from_secs(15);
/// Used only when neither the trade, the client nor a legacy venue names a source.
const DEFAULT_QUOTE_SOURCE: QuoteSource = QuoteSource::Relay;

impl TradingClient {
    pub fn quote_source_for(&self, trade: &Trade) -> QuoteSource {
        trade
            .quote_source
            .or(self.quote_source)
            .unwrap_or(match trade.venue {
                Some(Venue::PumpFun) => QuoteSource::PumpFun,
                Some(Venue::PumpSwap) => QuoteSource::PumpSwap,
                None => DEFAULT_QUOTE_SOURCE,
            })
    }

    fn source_adapter(&self, source: QuoteSource) -> &dyn Dex {
        match source {
            QuoteSource::Jupiter => &self.jupiter,
            QuoteSource::DFlow => &self.dflow,
            QuoteSource::Bloxroute => &self.bloxroute,
            QuoteSource::Relay => &self.relay,
            QuoteSource::PumpFun => &self.pumpfun,
            QuoteSource::PumpSwap => &self.pumpswap,
        }
    }

    pub(super) async fn prepare_selected_swap(&self, trade: &Trade) -> Result<PreparedSwap> {
        trade.validate()?;
        let source = self.quote_source_for(trade);
        if source.native_venue().is_some() && trade.settlement == Settlement::Usdc {
            return Err(TradeError::Build(
                "Pump.fun and PumpSwap trade SOL only; use an aggregator for USDC".into(),
            ));
        }
        let provider_fee = self.provider_fee(trade, source)?;
        // A provider-collected fee replaces the SDK transfer; never charge both.
        let transfer_fee = self.sdk_fee.filter(|_| provider_fee.is_none());

        let routed = self.routed_trade(trade, transfer_fee)?;
        let route = bounded_route(
            self.source_adapter(source).name(),
            self.prepare_route(source, &routed, provider_fee),
        );
        let (prepared, pool_prices) = tokio::join!(route, self.pool_prices(trade, source));
        let prepared = prepared?;
        validate_candidate(&routed, &prepared, provider_fee.is_some())?;
        let routed_quote = prepared.quote;
        let mut prepared = self.append_fees(trade, prepared, transfer_fee)?;
        prepared.quote.usd_value = if self.usd_value_enabled {
            pool_prices
                .and_then(|prices| prices.usd_value(trade, &prepared.quote))
                .or_else(|| usd_value_after_fees(&routed_quote, &prepared.quote))
        } else {
            None
        };
        Ok(prepared)
    }

    fn routed_trade(&self, trade: &Trade, transfer_fee: Option<SdkFee>) -> Result<Trade> {
        let routed = match transfer_fee {
            Some(fee) => fee.trade_after_fee(trade)?,
            None => *trade,
        };
        match self.sponsor_for(trade) {
            Some(sponsor) => sponsor.reserve_fee(&routed),
            None => Ok(routed),
        }
    }

    async fn prepare_route(
        &self,
        source: QuoteSource,
        trade: &Trade,
        provider_fee: Option<ProviderFee>,
    ) -> Result<PreparedSwap> {
        if let Some(fee) = provider_fee {
            return self.prepare_provider_fee_swap(source, trade, fee).await;
        }
        let adapter = self.source_adapter(source);
        match self.sponsor_for(trade) {
            Some(sponsor) => {
                adapter
                    .prepare_sponsored_swap(trade, &sponsor.wallet())
                    .await
            }
            None => adapter.prepare_swap(trade).await,
        }
        .map_err(|error| dex_err(adapter.name(), error))
    }

    /// Fee transfers go after the full route, so a failed transfer rolls back the swap.
    fn append_fees(
        &self,
        trade: &Trade,
        prepared: PreparedSwap,
        transfer_fee: Option<SdkFee>,
    ) -> Result<PreparedSwap> {
        let sponsor = self.sponsor_for(trade);
        let mut prepared = match transfer_fee {
            Some(fee) => {
                let payer = sponsor.map_or(trade.wallet, GasSponsor::wallet);
                fee.apply_with_payer(trade, prepared, &payer)?
            }
            None => prepared,
        };
        prepared.quote.in_amount = trade.amount;
        match sponsor {
            Some(sponsor) => sponsor.apply_to_swap(trade, prepared),
            None => Ok(prepared),
        }
    }

    fn provider_fee(&self, trade: &Trade, source: QuoteSource) -> Result<Option<ProviderFee>> {
        let Some(fee) = self
            .sdk_fee
            .filter(|fee| fee.collection() == FeeCollection::Provider && fee.basis_points() > 0)
        else {
            return Ok(None);
        };
        if !matches!(
            source,
            QuoteSource::Bloxroute | QuoteSource::DFlow | QuoteSource::Relay
        ) {
            return Err(TradeError::Build(
                "provider fee collection requires bloXroute, DFlow or Relay".into(),
            ));
        }
        ProviderFee::new(fee, trade).map(Some)
    }

    async fn prepare_provider_fee_swap(
        &self,
        source: QuoteSource,
        trade: &Trade,
        fee: ProviderFee,
    ) -> Result<PreparedSwap> {
        let payer = self.sponsor_for(trade).map(GasSponsor::wallet);
        match source {
            QuoteSource::Bloxroute if payer.is_none() => {
                self.bloxroute.prepare_with_fee(trade, Some(fee)).await
            }
            QuoteSource::Bloxroute => Err(TradeError::Build(
                "bloXroute sponsored swaps are unsupported".into(),
            )),
            QuoteSource::DFlow => {
                self.dflow
                    .prepare_with_fee(trade, payer.as_ref(), Some(fee))
                    .await
            }
            QuoteSource::Relay => {
                self.relay
                    .prepare_with_fee(trade, payer.as_ref(), Some(fee))
                    .await
            }
            _ => Err(TradeError::Build(
                "provider fee collection requires bloXroute, DFlow or Relay".into(),
            )),
        }
    }
}

fn validate_candidate(trade: &Trade, prepared: &PreparedSwap, provider_fee: bool) -> Result<()> {
    if prepared.quote.in_amount != trade.amount
        || (!provider_fee && prepared.quote.application_fee != 0)
        || prepared.quote.sponsorship_fee != 0
        || prepared.quote.min_out == 0
        || prepared.quote.min_out > prepared.quote.expected_out
        || prepared.instructions.is_empty()
    {
        return Err(TradeError::Build(format!(
            "{} returned invalid output amounts",
            prepared.venue
        )));
    }
    Ok(())
}

pub(super) async fn bounded_route<T>(
    venue: &'static str,
    preparation: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(ROUTE_PREPARATION_TIMEOUT, preparation)
        .await
        .map_err(|_| TradeError::Venue {
            venue,
            msg: "route preparation timed out".into(),
        })?
}
