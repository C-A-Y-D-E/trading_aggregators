use std::time::Duration;

use alloy_primitives::TxKind;

use super::node::ReceiptStatus;
use super::*;

const MAX_QUOTE_AGE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Extra gas over our own estimate; Relay-supplied limits are used as given.
const GAS_HEADROOM_PERCENT: u64 = 20;

impl<C: Network> Client<C> {
    /// Executes this prepared quote without requesting another one. Approvals may remain on-chain if the swap fails.
    pub async fn execute_swap(
        &self,
        prepared: PreparedSwap,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
    ) -> std::result::Result<SwapResult, SwapError> {
        let mut submitted = Vec::new();
        let result = self
            .execute_transactions(&prepared, signer, submitter, &mut submitted)
            .await;
        match result {
            Ok(()) => Ok(SwapResult {
                quote: prepared.quote,
                transaction_hashes: submitted,
            }),
            Err(source) => Err(SwapError { source, submitted }),
        }
    }

    async fn execute_transactions(
        &self,
        prepared: &PreparedSwap,
        signer: &dyn Signer,
        submitter: &dyn Submitter,
        submitted: &mut Vec<TransactionHash>,
    ) -> Result<()> {
        if prepared.quote.chain_id != C::CHAIN_ID {
            return Err(TradeError::Build(
                "prepared swap belongs to a different chain".into(),
            ));
        }
        self.check_node_chain().await?;
        for transaction in &prepared.transactions {
            if prepared.created_at.elapsed() >= MAX_QUOTE_AGE {
                return Err(TradeError::Build(
                    "prepared quote expired; no further transactions submitted".into(),
                ));
            }
            // Filled one at a time: a swap's gas can only be estimated once its approval is mined.
            let unsigned = self.fill(transaction).await?;
            let signed = signer
                .sign(&prepared.wallet, &unsigned)
                .await
                .map_err(|error| TradeError::Sign(format!("{error:#}")))?;
            let hash = unsigned.verify_signed(&signed, prepared.wallet)?;
            // Recorded before sending: a failed submit call may still have broadcast it.
            submitted.push(hash);
            let reported = submitter
                .submit(&signed)
                .await
                .map_err(|error| TradeError::Submit(format!("{error:#}")))?;
            if reported != hash {
                return Err(TradeError::Submit(format!(
                    "submitter reported {reported}, but the signed transaction is {hash}"
                )));
            }
            self.confirm(&hash).await?;
        }
        Ok(())
    }

    async fn check_node_chain(&self) -> Result<()> {
        let chain_id = self.node.chain_id().await?;
        if chain_id != C::CHAIN_ID {
            return Err(TradeError::Build(format!(
                "RPC is on chain {chain_id}, expected {} ({})",
                C::NAME,
                C::CHAIN_ID
            )));
        }
        Ok(())
    }

    async fn fill(&self, transaction: &Transaction) -> Result<UnsignedTransaction> {
        let gas_limit = match transaction.gas {
            Some(gas) => to_u64(gas, "gas limit")?,
            None => {
                let estimate = self.node.estimate_gas(transaction).await?;
                estimate + estimate * GAS_HEADROOM_PERCENT / 100
            }
        };
        let (max_fee_per_gas, max_priority_fee_per_gas) = self.fees(transaction).await?;
        Ok(UnsignedTransaction::new(TxEip1559 {
            chain_id: C::CHAIN_ID,
            nonce: self.node.pending_nonce(transaction.from).await?,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to: TxKind::Call(transaction.to),
            value: transaction.value,
            access_list: Default::default(),
            input: transaction.data.clone(),
        }))
    }

    /// Returns `(max_fee_per_gas, max_priority_fee_per_gas)`.
    async fn fees(&self, transaction: &Transaction) -> Result<(u128, u128)> {
        if let Some(gas_price) = transaction.gas_price {
            // A legacy gas price maps to an EIP-1559 fee cap and tip of the same value.
            let gas_price = to_u128(gas_price, "gas price")?;
            return Ok((gas_price, gas_price));
        }
        if let (Some(max_fee), Some(priority_fee)) = (
            transaction.max_fee_per_gas,
            transaction.max_priority_fee_per_gas,
        ) {
            return Ok((
                to_u128(max_fee, "max fee")?,
                to_u128(priority_fee, "priority fee")?,
            ));
        }
        let (estimated_max_fee, estimated_priority_fee) = self.node.eip1559_fees().await?;
        let priority_fee = match transaction.max_priority_fee_per_gas {
            Some(fee) => to_u128(fee, "priority fee")?,
            None => estimated_priority_fee,
        };
        let max_fee = match transaction.max_fee_per_gas {
            Some(fee) => to_u128(fee, "max fee")?,
            // The cap must cover the tip, even when only Relay's tip is used.
            None => estimated_max_fee.max(priority_fee),
        };
        Ok((max_fee, priority_fee))
    }

    async fn confirm(&self, hash: &TransactionHash) -> Result<()> {
        let poll = async {
            loop {
                match self.node.receipt_status(hash).await? {
                    ReceiptStatus::Succeeded => return Ok(()),
                    ReceiptStatus::Reverted => {
                        return Err(TradeError::Submit(format!("transaction {hash} reverted")));
                    }
                    ReceiptStatus::Pending => tokio::time::sleep(POLL_INTERVAL).await,
                }
            }
        };
        tokio::time::timeout(self.confirmation_timeout, poll)
            .await
            .map_err(|_| {
                TradeError::Submit(format!(
                    "confirmation timed out for {hash}; transaction may still confirm"
                ))
            })?
    }
}

fn to_u64(amount: Amount, field: &str) -> Result<u64> {
    amount
        .try_into()
        .map_err(|_| TradeError::Build(format!("{field} does not fit in 64 bits")))
}

fn to_u128(amount: Amount, field: &str) -> Result<u128> {
    amount
        .try_into()
        .map_err(|_| TradeError::Build(format!("{field} does not fit in 128 bits")))
}
