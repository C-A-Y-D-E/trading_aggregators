use std::error::Error as _;
use std::time::Duration;

use crate::{Result, TradeError};

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct ApiClient {
    pub http: reqwest::Client,
    pub base_url: String,
    venue: &'static str,
    authentication: Option<(&'static str, String)>,
}

impl ApiClient {
    pub fn new(venue: &'static str, base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            venue,
            authentication: None,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn with_authentication(mut self, header: &'static str, value: impl Into<String>) -> Self {
        self.authentication = Some((header, value.into()));
        self
    }

    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<T> {
        request = request.timeout(REQUEST_TIMEOUT);
        if let Some((header, value)) = &self.authentication {
            request = request.header(*header, value);
        }
        let response = request
            .send()
            .await
            .map_err(|source| self.send_error(source))?;
        if !response.status().is_success() {
            return Err(TradeError::Http {
                venue: self.venue,
                status: response.status().as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }
        response
            .json()
            .await
            .map_err(|error| TradeError::Decode(self.venue, error.to_string()))
    }

    /// A builder error means the configured URL is unusable, not that the network failed.
    fn send_error(&self, source: reqwest::Error) -> TradeError {
        if !source.is_builder() {
            return TradeError::Network {
                context: self.venue,
                source,
            };
        }
        TradeError::Config {
            what: self.venue,
            url: self.base_url.clone(),
            detail: source
                .source()
                .map_or_else(|| source.to_string(), ToString::to_string),
        }
    }
}
