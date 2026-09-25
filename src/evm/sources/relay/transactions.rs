use serde::Deserialize;
use serde_json::Value;

use super::{decimal, decode_error};
use crate::Result;
use crate::aggregators::relay::Step;
use crate::evm::{Address, Amount, Currency, Network, Trade, Transaction};

const APPROVE_SELECTOR: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ApiTransaction {
    chain_id: u64,
    from: Address,
    to: Address,
    data: alloy_primitives::Bytes,
    value: Value,
    gas: Option<Value>,
    gas_price: Option<Value>,
    max_fee_per_gas: Option<Value>,
    max_priority_fee_per_gas: Option<Value>,
}

pub(super) fn decode<C: Network>(
    steps: Vec<Step<ApiTransaction>>,
    trade: &Trade,
) -> Result<Vec<Transaction>> {
    if steps.is_empty() || steps.len() > 2 {
        return Err(decode_error(
            "expected optional approval followed by one same-chain swap",
        ));
    }
    let mut result = Vec::new();
    let count = steps.len();
    for (index, step) in steps.into_iter().enumerate() {
        let swap = index + 1 == count;
        if step.kind != "transaction"
            || (swap && step.id != "swap")
            || (!swap && !matches!(step.id.as_str(), "approve" | "approval"))
            || step.items.is_empty()
            || step.items.len() > if swap { 1 } else { 2 }
        {
            return Err(decode_error(
                "unsupported Relay step; only approvals and an atomic swap are supported",
            ));
        }
        for item in step.items {
            if !swap && item.status == "complete" {
                continue;
            }
            if item.status != "incomplete" {
                return Err(decode_error("transaction is not awaiting execution"));
            }
            let mut transaction = item
                .data
                .ok_or_else(|| decode_error("missing transaction data"))?
                .decode::<C>(trade)?;
            if swap {
                validate_swap::<C>(&transaction, trade)?;
            } else {
                validate_approval::<C>(&mut transaction, trade)?;
            }
            result.push(transaction);
        }
    }
    Ok(result)
}

impl ApiTransaction {
    fn decode<C: Network>(self, trade: &Trade) -> Result<Transaction> {
        if self.chain_id != C::CHAIN_ID
            || self.from != trade.wallet
            || self.to.is_zero()
            || self.data.len() < 4
        {
            return Err(decode_error(
                "invalid transaction chain, sender, target or calldata",
            ));
        }
        let transaction = Transaction {
            chain_id: self.chain_id,
            from: self.from,
            to: self.to,
            data: self.data,
            value: quantity(self.value)?,
            gas: self.gas.map(quantity).transpose()?,
            gas_price: self.gas_price.map(quantity).transpose()?,
            max_fee_per_gas: self.max_fee_per_gas.map(quantity).transpose()?,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.map(quantity).transpose()?,
        };
        if transaction.gas_price.is_some()
            && (transaction.max_fee_per_gas.is_some()
                || transaction.max_priority_fee_per_gas.is_some())
            || transaction
                .max_priority_fee_per_gas
                .zip(transaction.max_fee_per_gas)
                .is_some_and(|(priority, max)| priority > max)
        {
            return Err(decode_error("inconsistent transaction gas prices"));
        }
        Ok(transaction)
    }
}

fn validate_swap<C: Network>(transaction: &Transaction, trade: &Trade) -> Result<()> {
    let value = if trade.input == Currency::Native {
        trade.amount
    } else {
        Amount::ZERO
    };
    if ![C::ROUTER, C::APPROVAL_PROXY].contains(&transaction.to) || transaction.value != value {
        return Err(decode_error(
            "swap target or native value differs from the requested route",
        ));
    }
    Ok(())
}

fn validate_approval<C: Network>(transaction: &mut Transaction, trade: &Trade) -> Result<()> {
    if trade.input == Currency::Native
        || transaction.to != trade.input.address()
        || !transaction.value.is_zero()
        || transaction.data.len() != 68
        || transaction.data[..4] != APPROVE_SELECTOR
        || transaction.data[4..16] != [0; 12]
    {
        return Err(decode_error("invalid token approval"));
    }
    let spender = Address::from_slice(&transaction.data[16..36]);
    let allowance = Amount::from_be_slice(&transaction.data[36..68]);
    if ![C::ROUTER, C::APPROVAL_PROXY].contains(&spender)
        || (!allowance.is_zero() && allowance < trade.amount)
    {
        return Err(decode_error(
            "unexpected approval spender or insufficient allowance",
        ));
    }
    if !allowance.is_zero() {
        // Bound the new allowance to this trade instead of signing an unlimited approval from the provider.
        let mut data = transaction.data.to_vec();
        data[36..68].copy_from_slice(&trade.amount.to_be_bytes::<32>());
        transaction.data = data.into();
    }
    Ok(())
}

fn quantity(value: Value) -> Result<Amount> {
    match value {
        Value::String(value) => match value.strip_prefix("0x") {
            Some(hex) if !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
                Amount::from_str_radix(hex, 16).map_err(decode_error)
            }
            _ => decimal(&value),
        },
        Value::Number(value) => value
            .as_u64()
            .map(Amount::from)
            .ok_or_else(|| decode_error("invalid numeric transaction quantity")),
        _ => Err(decode_error("invalid transaction quantity")),
    }
}
