use crate::{Result, TradeError, robinhood, solana};

#[derive(Default)]
pub struct TradingAggregator {
    solana: Option<solana::Client>,
    robinhood: Option<robinhood::Client>,
}

impl TradingAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_solana(mut self, client: solana::Client) -> Self {
        self.solana = Some(client);
        self
    }

    pub fn with_robinhood(mut self, client: robinhood::Client) -> Self {
        self.robinhood = Some(client);
        self
    }

    pub fn solana(&self) -> Result<&solana::Client> {
        self.solana
            .as_ref()
            .ok_or_else(|| TradeError::Build("Solana is not configured".into()))
    }

    pub fn robinhood(&self) -> Result<&robinhood::Client> {
        self.robinhood
            .as_ref()
            .ok_or_else(|| TradeError::Build("Robinhood is not configured".into()))
    }
}
