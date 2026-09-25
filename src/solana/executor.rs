use std::sync::Arc;
use std::time::{Duration, Instant};

use borsh::BorshDeserialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSimulateTransactionConfig;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_message::{AddressLookupTableAccount, Hash, VersionedMessage, v0, v1};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::error::{Result, TradeError};
use crate::solana::dexes::common::{
    COMPUTE_BUDGET_PROGRAM, REQUEST_HEAP_FRAME, SET_COMPUTE_UNIT_LIMIT,
    SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT, TOKEN_PROGRAM, ata, decode_account,
    set_compute_unit_limit, set_compute_unit_price, tip as tip_ix,
};
use crate::solana::lookup_table::merge_lookup_tables;
use crate::solana::types::{
    PreparedSwap, Settlement, Side, Signer, Submitter, SwapResult, SwapStatus, Trade,
    TransactionFormat,
};
use crate::{GasSponsor, USDC_MINT};

const CU_LIMIT_MAX: u32 = 1_400_000;
const DEFAULT_CU_LIMIT: u32 = 350_000;
/// 20% above simulated usage, so small state changes before landing don't exhaust compute.
const CU_HEADROOM: f64 = 1.2;
const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;
const MAX_V0_TRANSACTION_BYTES: u64 = 1_232;
/// V0's default when no limit is requested; V1 would read a missing limit as zero.
const MAX_LOADED_ACCOUNTS_DATA_SIZE: u32 = 64 * 1024 * 1024;
/// V0 layout is [CU limit, CU price, ...route], so route instruction `i` lands at `i + 2`.
/// Sponsorship only builds V0 transactions.
const PREPENDED_BUDGET_INSTRUCTIONS: usize = 2;

/// One transaction's route instructions and the layout to build them in.
pub(crate) struct Route<'a> {
    pub instructions: Vec<Instruction>,
    pub lookup_tables: &'a [AddressLookupTableAccount],
    pub format: TransactionFormat,
}

pub(crate) struct SwapSigners<'a> {
    pub user: &'a dyn Signer,
    pub sponsor: Option<&'a GasSponsor>,
}

impl SwapSigners<'_> {
    fn payer(&self, wallet: &Pubkey) -> Pubkey {
        self.sponsor.map_or(*wallet, GasSponsor::wallet)
    }

    fn user_signer_index(&self, wallet: &Pubkey, tx: &VersionedTransaction) -> Result<usize> {
        let required = usize::from(tx.message.header().num_required_signatures);
        let signers = &tx.message.static_account_keys()[..required];
        let payer = self.payer(wallet);
        if signers.first() != Some(&payer)
            || !signers.contains(wallet)
            || signers.iter().any(|key| *key != *wallet && *key != payer)
        {
            return Err(TradeError::Sign(
                "transaction requires unexpected signers".into(),
            ));
        }
        Ok(signers.iter().position(|key| key == wallet).unwrap())
    }

    async fn sign(&self, wallet: &Pubkey, tx: &mut VersionedTransaction) -> Result<()> {
        let user_index = self.user_signer_index(wallet, tx)?;
        tx.signatures[user_index] = self
            .user
            .sign(wallet, tx)
            .await
            .map_err(|error| TradeError::Sign(format!("user: {error:#}")))?;
        if let Some(sponsor) = self.sponsor {
            tx.signatures[0] = sponsor
                .signer
                .sign(&sponsor.wallet(), tx)
                .await
                .map_err(|error| TradeError::Sign(format!("sponsor: {error:#}")))?;
        }
        Ok(())
    }
}

pub(crate) fn dex_err(venue: &'static str, e: anyhow::Error) -> TradeError {
    match e.downcast::<TradeError>() {
        Ok(te) => te,
        Err(e) => TradeError::Venue {
            venue,
            msg: format!("{e:#}"),
        },
    }
}

pub(crate) async fn submit_swap(
    rpc: &Arc<RpcClient>,
    prepared: PreparedSwap,
    signers: SwapSigners<'_>,
    submitter: &dyn Submitter,
    params: &Trade,
    priority_fee_lamports: u64,
    supplemental_lookup_tables: &[AddressLookupTableAccount],
) -> Result<SwapResult> {
    let alts = merge_lookup_tables(&prepared.lookup_tables, supplemental_lookup_tables);
    let reimbursement_index = prepared
        .instructions
        .iter()
        .filter(|instruction| !is_compute_unit_limit(instruction))
        .count()
        .checked_sub(1)
        .map(|index| index + PREPENDED_BUDGET_INSTRUCTIONS);
    let mut sponsorship_fee = prepared.quote.sponsorship_fee;
    let route = Route {
        instructions: prepared.instructions,
        lookup_tables: &alts,
        format: prepared.format,
    };
    let mut tx = build_transaction(
        rpc,
        &signers.payer(&params.wallet),
        route,
        priority_fee_lamports,
        submitter,
    )
    .await?;
    signers.user_signer_index(&params.wallet, &tx)?;
    if let Some(sponsor) = signers.sponsor {
        sponsorship_fee = sponsor
            .cost_policy
            .finalize_fee(
                rpc,
                &mut tx,
                reimbursement_index
                    .ok_or_else(|| TradeError::Build("missing sponsorship transfer".into()))?,
            )
            .await?;
    }
    signers.sign(&params.wallet, &mut tx).await?;
    let sig = submitter
        .submit(&tx)
        .await
        .map_err(|e| TradeError::Submit(format!("{e:#}")))?;
    Ok(SwapResult {
        hash: sig.to_string(),
        dex: prepared.venue,
        status: SwapStatus::Pending,
        amount_received: None,
        application_fee: prepared.quote.application_fee,
        sponsorship_fee,
    })
}

pub(crate) async fn build_transaction(
    rpc: &Arc<RpcClient>,
    payer: &Pubkey,
    mut route: Route<'_>,
    priority_fee_lamports: u64,
    submitter: &dyn Submitter,
) -> Result<VersionedTransaction> {
    let budget = RequestedBudget::take(&mut route.instructions, route.format)?;
    if let Some(t) = submitter.default_tip() {
        route
            .instructions
            .push(tip_ix(payer, &t.account, t.lamports));
    }

    let (simulated, blockhash) = tokio::join!(
        simulate_units(rpc, payer, &route, &budget),
        rpc.get_latest_blockhash()
    );
    let cu_limit = match simulated? {
        Some(units) => ((units as f64 * CU_HEADROOM) as u32).clamp(1, CU_LIMIT_MAX),
        // Only a successful simulation missing its CU estimate uses the default.
        None => DEFAULT_CU_LIMIT,
    }
    .max(budget.compute_unit_limit.unwrap_or(0));
    let blockhash = blockhash.map_err(|source| TradeError::Rpc {
        context: "get_latest_blockhash",
        source,
    })?;
    let message = match route.format {
        TransactionFormat::V0 => {
            // The caller budgets a total priority fee; V0 prices it per compute unit.
            let price = (u128::from(priority_fee_lamports) * MICRO_LAMPORTS_PER_LAMPORT
                / u128::from(cu_limit)) as u64;
            let mut all = vec![
                set_compute_unit_limit(cu_limit),
                set_compute_unit_price(price),
            ];
            all.extend(route.instructions);
            compile_v0(payer, &all, route.lookup_tables, blockhash)?
        }
        TransactionFormat::V1 => {
            let config = budget.v1_config(cu_limit, priority_fee_lamports);
            compile_v1(payer, &route.instructions, config, blockhash)?
        }
    };
    let tx = unsigned(message);
    check_size(&tx, route.format)?;
    Ok(tx)
}

/// Compute budget a route asks for. V0 keeps heap and loaded-account requests as
/// instructions; V1 moves them into the message header, where they must live.
#[derive(Default)]
struct RequestedBudget {
    compute_unit_limit: Option<u32>,
    loaded_accounts_data_size_limit: Option<u32>,
    heap_size: Option<u32>,
}

impl RequestedBudget {
    fn take(instructions: &mut Vec<Instruction>, format: TransactionFormat) -> Result<Self> {
        let mut budget = Self {
            compute_unit_limit: take_compute_unit_limit(instructions)?,
            ..Self::default()
        };
        if format == TransactionFormat::V0 {
            return Ok(budget);
        }
        for instruction in instructions
            .iter()
            .filter(|instruction| instruction.program_id == COMPUTE_BUDGET_PROGRAM)
        {
            let (tag, value) = budget_value(instruction)?;
            let slot = match tag {
                REQUEST_HEAP_FRAME => &mut budget.heap_size,
                SET_LOADED_ACCOUNTS_DATA_SIZE_LIMIT => &mut budget.loaded_accounts_data_size_limit,
                _ => return Err(invalid_budget()),
            };
            if slot.replace(value).is_some() {
                return Err(invalid_budget());
            }
        }
        instructions.retain(|instruction| instruction.program_id != COMPUTE_BUDGET_PROGRAM);
        Ok(budget)
    }

    /// V1 takes the priority fee as total lamports, the unit callers already budget in.
    fn v1_config(
        &self,
        compute_unit_limit: u32,
        priority_fee_lamports: u64,
    ) -> v1::TransactionConfig {
        v1::TransactionConfig {
            priority_fee: (priority_fee_lamports > 0).then_some(priority_fee_lamports),
            compute_unit_limit: Some(compute_unit_limit),
            loaded_accounts_data_size_limit: Some(
                self.loaded_accounts_data_size_limit
                    .unwrap_or(MAX_LOADED_ACCOUNTS_DATA_SIZE),
            ),
            heap_size: self.heap_size,
        }
    }
}

fn budget_value(instruction: &Instruction) -> Result<(u8, u32)> {
    let [tag, value @ ..] = instruction.data.as_slice() else {
        return Err(invalid_budget());
    };
    let value: [u8; 4] = value.try_into().map_err(|_| invalid_budget())?;
    Ok((*tag, u32::from_le_bytes(value)))
}

fn invalid_budget() -> TradeError {
    TradeError::Build("invalid or duplicate compute-budget request for a v1 transaction".into())
}

fn compile_v0(
    payer: &Pubkey,
    instructions: &[Instruction],
    lookup_tables: &[AddressLookupTableAccount],
    blockhash: Hash,
) -> Result<VersionedMessage> {
    v0::Message::try_compile(payer, instructions, lookup_tables, blockhash)
        .map(VersionedMessage::V0)
        .map_err(|error| TradeError::Build(format!("compile message: {error}")))
}

fn compile_v1(
    payer: &Pubkey,
    instructions: &[Instruction],
    config: v1::TransactionConfig,
    blockhash: Hash,
) -> Result<VersionedMessage> {
    let message = v1::Message::try_compile_with_config(payer, instructions, blockhash, config)
        .map_err(|error| TradeError::Build(format!("compile v1 message: {error}")))?;
    // Compiling doesn't enforce V1's 64-account and 64-instruction caps; validation does.
    message
        .validate()
        .map_err(|error| TradeError::Build(format!("invalid v1 message: {error:?}")))?;
    Ok(VersionedMessage::V1(message))
}

fn unsigned(message: VersionedMessage) -> VersionedTransaction {
    VersionedTransaction {
        signatures: vec![
            Signature::default();
            usize::from(message.header().num_required_signatures)
        ],
        message,
    }
}

/// Measured in wincode, the wire encoding; bincode lays V1 out wrongly.
fn check_size(tx: &VersionedTransaction, format: TransactionFormat) -> Result<()> {
    let (maximum, hint) = match format {
        TransactionFormat::V0 => (
            MAX_V0_TRANSACTION_BYTES,
            "; provide an address lookup table",
        ),
        TransactionFormat::V1 => (v1::MAX_TRANSACTION_SIZE as u64, ""),
    };
    let size = wincode::serialized_size(tx)
        .map_err(|error| TradeError::Build(format!("measure transaction: {error}")))?;
    if size > maximum {
        return Err(TradeError::Build(format!(
            "transaction is {size} bytes; maximum is {maximum}{hint}"
        )));
    }
    Ok(())
}

fn is_compute_unit_limit(instruction: &Instruction) -> bool {
    instruction.program_id == COMPUTE_BUDGET_PROGRAM
        && instruction.data.first() == Some(&SET_COMPUTE_UNIT_LIMIT)
}

fn take_compute_unit_limit(instructions: &mut Vec<Instruction>) -> Result<Option<u32>> {
    let mut requested = None;
    for instruction in instructions
        .iter()
        .filter(|instruction| is_compute_unit_limit(instruction))
    {
        let [SET_COMPUTE_UNIT_LIMIT, bytes @ ..] = instruction.data.as_slice() else {
            unreachable!("filtered to compute unit limit instructions")
        };
        let bytes: [u8; 4] = bytes
            .try_into()
            .map_err(|_| TradeError::Build("invalid compute unit limit".into()))?;
        let limit = u32::from_le_bytes(bytes);
        if requested.is_some()
            || !(1..=CU_LIMIT_MAX).contains(&limit)
            || !instruction.accounts.is_empty()
        {
            return Err(TradeError::Build(
                "invalid or duplicate compute unit limit".into(),
            ));
        }
        requested = Some(limit);
    }
    instructions.retain(|instruction| !is_compute_unit_limit(instruction));
    Ok(requested)
}

async fn simulate_units(
    rpc: &Arc<RpcClient>,
    payer: &Pubkey,
    route: &Route<'_>,
    budget: &RequestedBudget,
) -> Result<Option<u64>> {
    // Placeholder: `replace_recent_blockhash` below makes the node substitute a live blockhash.
    let message = match route.format {
        TransactionFormat::V0 => {
            let mut sim_ixs = vec![set_compute_unit_limit(CU_LIMIT_MAX)];
            sim_ixs.extend_from_slice(&route.instructions);
            compile_v0(payer, &sim_ixs, route.lookup_tables, Hash::default())?
        }
        TransactionFormat::V1 => {
            let config = budget.v1_config(CU_LIMIT_MAX, 0);
            compile_v1(payer, &route.instructions, config, Hash::default())?
        }
    };
    let tx = unsigned(message);
    let sim = rpc
        .simulate_transaction_with_config(
            &tx,
            RpcSimulateTransactionConfig {
                sig_verify: false,
                replace_recent_blockhash: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|source| TradeError::Rpc {
            context: "simulate_transaction",
            source,
        })?
        .value;
    if let Some(error) = sim.err {
        return Err(TradeError::Simulation {
            error: format!("{error:?}"),
            logs: sim.logs.unwrap_or_default(),
        });
    }
    Ok(sim.units_consumed)
}

pub(crate) async fn check_status(rpc: &Arc<RpcClient>, sig: &Signature) -> Result<SwapStatus> {
    let status = rpc
        .get_signature_status_with_commitment(sig, CommitmentConfig::confirmed())
        .await
        .map_err(|source| TradeError::Rpc {
            context: "signature status",
            source,
        })?;
    Ok(match status {
        Some(Ok(())) => SwapStatus::Confirmed,
        Some(Err(_)) => SwapStatus::Failed,
        None => SwapStatus::Pending,
    })
}

pub(crate) async fn confirm(
    rpc: &Arc<RpcClient>,
    sig: &Signature,
    deadline: Duration,
) -> Result<SwapStatus> {
    let start = Instant::now();
    loop {
        match check_status(rpc, sig).await? {
            SwapStatus::Pending => {
                if start.elapsed() >= deadline {
                    return Ok(SwapStatus::Pending);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            terminal => return Ok(terminal),
        }
    }
}

/// Leading fields shared by SPL Token and Token-2022 accounts.
#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct TokenAccountHead {
    mint: Pubkey,
    owner: Pubkey,
    amount: u64,
}

/// Reads at `confirmed`, the commitment `confirm` waits for; a finalized read still shows
/// the pre-swap balance. `None` means unknown, not zero.
pub(crate) async fn output_balance(rpc: &RpcClient, trade: &Trade) -> Option<u64> {
    let account = match (trade.side, trade.settlement) {
        (Side::Sell, Settlement::Sol) => {
            let balance = rpc
                .get_balance_with_commitment(&trade.wallet, CommitmentConfig::confirmed())
                .await
                .ok()?;
            return Some(balance.value);
        }
        (Side::Sell, Settlement::Usdc) => ata(&trade.wallet, &USDC_MINT, &TOKEN_PROGRAM),
        (Side::Buy, _) => {
            let mint = rpc.get_account(&trade.mint).await.ok()?;
            ata(&trade.wallet, &trade.mint, &mint.owner)
        }
    };
    token_balance(rpc, &account).await.ok()
}

/// A missing token account holds zero.
pub(crate) async fn token_balance(rpc: &RpcClient, account: &Pubkey) -> Result<u64> {
    let response = rpc
        .get_account_with_commitment(account, CommitmentConfig::confirmed())
        .await
        .map_err(|source| TradeError::Rpc {
            context: "token balance",
            source,
        })?;
    let Some(account) = response.value else {
        return Ok(0);
    };
    token_account_amount(&account.data)
        .map_err(|error| TradeError::Decode("token account", error.to_string()))
}

pub(crate) fn token_account_amount(data: &[u8]) -> anyhow::Result<u64> {
    Ok(decode_account::<TokenAccountHead>(data)?.amount)
}
