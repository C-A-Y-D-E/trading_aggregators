use alloy_consensus::transaction::{RlpEcdsaDecodableTx, RlpEcdsaEncodableTx};
use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{B256, Bytes, Signature};

use super::{Address, TransactionHash};
use crate::{Result, TradeError};

/// A filled EIP-1559 transaction (nonce, gas and fees set) waiting for the wallet's signature.
#[derive(Debug, Clone)]
pub struct UnsignedTransaction {
    transaction: TxEip1559,
}

impl UnsignedTransaction {
    pub(super) fn new(transaction: TxEip1559) -> Self {
        Self { transaction }
    }

    pub fn transaction(&self) -> &TxEip1559 {
        &self.transaction
    }

    /// `0x02 || rlp(fields)`: the payload KMS signers such as Turnkey's `sign_transaction` take.
    pub fn encoded_for_signing(&self) -> Vec<u8> {
        self.transaction.encoded_for_signing()
    }

    /// keccak256 of `encoded_for_signing`, for signers that sign a raw digest.
    pub fn signing_hash(&self) -> B256 {
        self.transaction.signature_hash()
    }

    /// Raw signed bytes for a digest signature, ready to return from a `Signer`.
    pub fn encode_signed(&self, signature: &Signature) -> Bytes {
        let mut signed = Vec::new();
        self.transaction.eip2718_encode(signature, &mut signed);
        signed.into()
    }

    pub(super) fn verify_signed(&self, signed: &[u8], wallet: Address) -> Result<TransactionHash> {
        let mut remaining = signed;
        let decoded = TxEip1559::eip2718_decode(&mut remaining)
            .map_err(|error| sign_error(format!("signed bytes are not EIP-1559: {error}")))?;
        if !remaining.is_empty() || decoded.tx() != &self.transaction {
            return Err(sign_error(
                "signed transaction differs from the prepared one".into(),
            ));
        }
        let signer = decoded
            .recover_signer()
            .map_err(|error| sign_error(format!("cannot recover signer: {error}")))?;
        if signer != wallet {
            return Err(sign_error(format!(
                "signed by {signer}, expected trading wallet {wallet}"
            )));
        }
        Ok(*decoded.hash())
    }
}

fn sign_error(message: String) -> TradeError {
    TradeError::Sign(message)
}
