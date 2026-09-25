use alloy_primitives::{Bytes, hex};
use alloy_provider::{Provider, RootProvider};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::node::{Node, connect, rpc_error};
use super::{Address, Submitter, TransactionHash};
use crate::Result;

pub const BLOXROUTE_DEFAULT_URL: &str = "https://api.blxrbdn.com";

const BLOXROUTE_VENUE: &str = "bloxroute";
const BLOXROUTE_METHOD: &str = "robinhood_tx";

pub struct RpcSubmitter {
    node: Node,
}

impl RpcSubmitter {
    pub fn new(rpc_url: &str) -> Result<Self> {
        Ok(Self {
            node: Node::new(rpc_url)?,
        })
    }
}

#[async_trait]
impl Submitter for RpcSubmitter {
    async fn submit(&self, signed: &Bytes) -> anyhow::Result<TransactionHash> {
        Ok(self.node.send_raw_transaction(signed).await?)
    }
}

/// Transactions sent through bloXroute are also eligible for BackRunMe revenue sharing.
pub struct BloxrouteSubmitter {
    provider: RootProvider,
    auth_header: String,
    backrun_reward_address: Option<Address>,
}

impl BloxrouteSubmitter {
    pub fn new(auth_header: impl Into<String>) -> Result<Self> {
        let auth_header = auth_header.into();
        Ok(Self {
            provider: connect(BLOXROUTE_VENUE, BLOXROUTE_DEFAULT_URL, Some(&auth_header))?,
            auth_header,
            backrun_reward_address: None,
        })
    }

    pub fn with_url(mut self, url: &str) -> Result<Self> {
        self.provider = connect(BLOXROUTE_VENUE, url, Some(&self.auth_header))?;
        Ok(self)
    }

    pub fn with_backrun_reward_address(mut self, address: Address) -> Self {
        self.backrun_reward_address = Some(address);
        self
    }
}

#[async_trait]
impl Submitter for BloxrouteSubmitter {
    async fn submit(&self, signed: &Bytes) -> anyhow::Result<TransactionHash> {
        let params = BloxrouteParams {
            transaction: hex::encode(signed),
            // Without node validation a bad nonce or sender is dropped silently instead of reported.
            node_validation: true,
            backrunme_reward_address: self.backrun_reward_address,
        };
        let result: BloxrouteResult = self
            .provider
            .raw_request(BLOXROUTE_METHOD.into(), params)
            .await
            .map_err(rpc_error(BLOXROUTE_VENUE, BLOXROUTE_METHOD))?;
        Ok(result.tx_hash)
    }
}

#[derive(Clone, Debug, Serialize)]
struct BloxrouteParams {
    /// Hex without the `0x` prefix, as bloXroute requires.
    transaction: String,
    node_validation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    backrunme_reward_address: Option<Address>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BloxrouteResult {
    tx_hash: TransactionHash,
}
