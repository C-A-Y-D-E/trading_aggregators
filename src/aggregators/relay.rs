//! Relay's quote API, shared by both chains. `A` is the chain's address type and `S` the
//! transaction data inside each step; the chain adapters check and decode both.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::http::ApiClient;
use crate::{Result, TradeError, UsdValue};

pub const DEFAULT_BASE_URL: &str = "https://api.relay.link";

pub(crate) const VENUE: &str = "relay";
const QUOTE_PATH: &str = "/quote/v2";
const API_KEY_HEADER: &str = "x-api-key";
const EXACT_INPUT: &str = "EXACT_INPUT";

pub(crate) struct RelayApi {
    api: ApiClient,
}

impl RelayApi {
    pub fn new() -> Self {
        Self {
            api: ApiClient::new(VENUE, DEFAULT_BASE_URL),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.api = self.api.with_base_url(base_url);
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api = self.api.with_authentication(API_KEY_HEADER, api_key);
        self
    }

    pub async fn quote<A, X, S>(&self, request: &QuoteRequest<A, X>) -> Result<QuoteResponse<A, S>>
    where
        A: Serialize + DeserializeOwned,
        X: Serialize,
        S: DeserializeOwned,
    {
        let url = format!("{}{QUOTE_PATH}", self.api.base_url);
        self.api
            .request(self.api.http.post(url).json(request))
            .await
    }
}

/// One same-chain exact-input swap that pays back to the same wallet.
pub(crate) struct Swap<A> {
    pub wallet: A,
    pub chain_id: u64,
    pub input: A,
    pub output: A,
    pub amount: String,
    pub slippage_bps: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuoteRequest<A, X> {
    user: A,
    recipient: A,
    origin_chain_id: u64,
    destination_chain_id: u64,
    origin_currency: A,
    destination_currency: A,
    amount: String,
    trade_type: &'static str,
    slippage_tolerance: String,
    force_solver_execution: bool,
    #[serde(flatten)]
    chain_options: X,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    app_fees: Vec<AppFeeRequest<A>>,
}

impl<A: Clone, X> QuoteRequest<A, X> {
    pub fn new(swap: &Swap<A>, chain_options: X, app_fees: Vec<AppFeeRequest<A>>) -> Self {
        Self {
            user: swap.wallet.clone(),
            recipient: swap.wallet.clone(),
            origin_chain_id: swap.chain_id,
            destination_chain_id: swap.chain_id,
            origin_currency: swap.input.clone(),
            destination_currency: swap.output.clone(),
            amount: swap.amount.clone(),
            trade_type: EXACT_INPUT,
            slippage_tolerance: swap.slippage_bps.clone(),
            force_solver_execution: false,
            chain_options,
            app_fees,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct AppFeeRequest<A> {
    pub recipient: A,
    pub fee: String,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "A: Deserialize<'de>, S: Deserialize<'de>"))]
pub(crate) struct QuoteResponse<A, S> {
    pub details: Details<A>,
    pub steps: Vec<Step<S>>,
    #[serde(default)]
    pub fees: Fees<A>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Details<A> {
    pub operation: String,
    pub sender: A,
    pub recipient: A,
    pub currency_in: CurrencyAmount<A>,
    pub currency_out: CurrencyAmount<A>,
}

impl<A: PartialEq> Details<A> {
    /// Relay echoes the swap it quoted; anything else is not the requested same-chain swap.
    pub fn matches(&self, swap: &Swap<A>) -> bool {
        self.operation == "swap"
            && self.sender == swap.wallet
            && self.recipient == swap.wallet
            && self.currency_in.currency.chain_id == swap.chain_id
            && self.currency_out.currency.chain_id == swap.chain_id
            && self.currency_in.currency.address == swap.input
            && self.currency_out.currency.address == swap.output
    }

    pub fn usd_value(&self) -> Option<UsdValue> {
        UsdValue::from_reported(self.currency_in.usd(), self.currency_out.usd())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CurrencyAmount<A> {
    pub currency: Currency<A>,
    pub amount: String,
    pub minimum_amount: String,
    amount_usd: Option<String>,
}

impl<A> CurrencyAmount<A> {
    /// USD is display-only, so an unparseable value is dropped instead of failing the quote.
    fn usd(&self) -> Option<f64> {
        self.amount_usd.as_deref()?.parse().ok()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Currency<A> {
    pub chain_id: u64,
    pub address: A,
}

#[derive(Deserialize)]
pub(crate) struct Step<S> {
    pub id: String,
    pub kind: String,
    pub items: Vec<StepItem<S>>,
}

#[derive(Deserialize)]
pub(crate) struct StepItem<S> {
    pub status: String,
    pub data: Option<S>,
}

#[derive(Deserialize)]
pub(crate) struct Fees<A> {
    pub app: Option<CurrencyAmount<A>>,
}

impl<A> Default for Fees<A> {
    fn default() -> Self {
        Self { app: None }
    }
}

pub(crate) fn decode_error(error: impl std::fmt::Display) -> TradeError {
    TradeError::Decode(VENUE, error.to_string())
}
