use std::fmt::Display;

use alloy_primitives::Bytes;
use alloy_provider::network::{Ethereum, Network, TransactionBuilder};
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

use super::{Address, Transaction, TransactionHash};
use crate::http::REQUEST_TIMEOUT;
use crate::{Result, TradeError};

const VENUE: &str = "evm rpc";

type TransactionRequest = <Ethereum as Network>::TransactionRequest;

pub(super) fn connect(
    venue: &'static str,
    url: &str,
    authorization: Option<&str>,
) -> Result<RootProvider> {
    let config_error = |detail: String| TradeError::Config {
        what: venue,
        url: url.to_owned(),
        detail,
    };
    let parsed = url
        .parse()
        .map_err(|error| config_error(format!("{error}")))?;
    let mut headers = HeaderMap::new();
    if let Some(value) = authorization {
        let mut value =
            HeaderValue::from_str(value).map_err(|error| config_error(error.to_string()))?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    }
    Ok(ProviderBuilder::new()
        .disable_recommended_fillers()
        .with_reqwest(parsed, |builder| {
            // Building fails only if TLS cannot start, and the default client fails the same way.
            builder
                .timeout(REQUEST_TIMEOUT)
                .default_headers(headers)
                .build()
                .unwrap_or_default()
        }))
}

pub(super) struct Node {
    provider: RootProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReceiptStatus {
    Pending,
    Succeeded,
    Reverted,
}

impl Node {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            provider: connect(VENUE, url, None)?,
        })
    }

    pub async fn chain_id(&self) -> Result<u64> {
        self.provider
            .get_chain_id()
            .await
            .map_err(rpc_error(VENUE, "eth_chainId"))
    }

    /// Counts pending transactions too, so a still-unmined earlier swap is not overwritten.
    pub async fn pending_nonce(&self, address: Address) -> Result<u64> {
        self.provider
            .get_transaction_count(address)
            .pending()
            .await
            .map_err(rpc_error(VENUE, "eth_getTransactionCount"))
    }

    pub async fn estimate_gas(&self, transaction: &Transaction) -> Result<u64> {
        let request = TransactionRequest::default()
            .with_from(transaction.from)
            .with_to(transaction.to)
            .with_value(transaction.value)
            .with_input(transaction.data.clone());
        self.provider
            .estimate_gas(request)
            .await
            .map_err(rpc_error(VENUE, "eth_estimateGas"))
    }

    /// Returns `(max_fee_per_gas, max_priority_fee_per_gas)` from recent fee history.
    pub async fn eip1559_fees(&self) -> Result<(u128, u128)> {
        let estimate = self
            .provider
            .estimate_eip1559_fees()
            .await
            .map_err(rpc_error(VENUE, "eth_feeHistory"))?;
        Ok((estimate.max_fee_per_gas, estimate.max_priority_fee_per_gas))
    }

    pub async fn receipt_status(&self, hash: &TransactionHash) -> Result<ReceiptStatus> {
        let receipt = self
            .provider
            .get_transaction_receipt(*hash)
            .await
            .map_err(rpc_error(VENUE, "eth_getTransactionReceipt"))?;
        Ok(match receipt {
            None => ReceiptStatus::Pending,
            Some(receipt) if receipt.status() => ReceiptStatus::Succeeded,
            Some(_) => ReceiptStatus::Reverted,
        })
    }

    pub async fn send_raw_transaction(&self, signed: &Bytes) -> Result<TransactionHash> {
        let pending = self
            .provider
            .send_raw_transaction(signed)
            .await
            .map_err(rpc_error(VENUE, "eth_sendRawTransaction"))?;
        Ok(*pending.tx_hash())
    }
}

pub(super) fn rpc_error<E: Display>(
    venue: &'static str,
    method: &'static str,
) -> impl FnOnce(E) -> TradeError {
    move |error| TradeError::Venue {
        venue,
        msg: format!("{method}: {error}"),
    }
}
