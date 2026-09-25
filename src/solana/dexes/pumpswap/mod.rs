use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use borsh::BorshDeserialize;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::{Pubkey, pubkey};

use crate::solana::dexes::common::*;
use crate::solana::types::{
    Dex, NativeMarket, PreparedSwap, Quote, QuoteSource, Settlement, Side, Trade,
};

pub const PROGRAM_ID: Pubkey = pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
pub const FEE_PROGRAM_ID: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const DEFAULT_SOL_USDC_POOL: Pubkey = pubkey!("Gf7sXMoP8iRw4iiXmJ1nq4vxcRycbGXy5RL8a8LnTd3v");

const PUMPFUN_PROGRAM_ID: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");

pub(crate) const BUY_EXACT_QUOTE_IN_IX: &str = "buy_exact_quote_in";
pub(crate) const SELL_IX: &str = "sell";
const SOL_ONLY: &str = "pumpswap trades SOL-paired pools only; use an aggregator for USDC";
/// Graduation always creates pool index 0 for a token.
const CANONICAL_POOL_INDEX: u16 = 0;

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct PoolAccount {
    discriminator: u64,
    pool_bump: u8,
    index: u16,
    creator: Pubkey,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    lp_mint: Pubkey,
    pool_base_token_account: Pubkey,
    pool_quote_token_account: Pubkey,
    lp_supply: u64,
    coin_creator: Pubkey,
    is_mayhem_mode: bool,
    is_cashback_coin: bool,
}

fn decode_pool(data: &[u8]) -> Result<(PoolAccount, i128)> {
    let mut remaining = data;
    let pool = PoolAccount::deserialize_reader(&mut remaining)?;
    // Older accounts predate the appended virtual quote reserve field.
    let virtual_quote_reserves = if remaining.is_empty() {
        0
    } else {
        i128::deserialize_reader(&mut remaining)?
    };
    Ok((pool, virtual_quote_reserves))
}

fn effective_quote_reserves(vault_balance: u64, virtual_reserves: i128) -> Result<u64> {
    let reserves = i128::from(vault_balance)
        .checked_add(virtual_reserves)
        .ok_or_else(|| anyhow!("pumpswap: effective quote reserves overflow"))?;
    u64::try_from(reserves)
        .map_err(|_| anyhow!("pumpswap: effective quote reserves outside u64 range"))
}

/// The vaults and virtual reserves behind a pool's spot price.
pub(crate) struct PoolVaults {
    pub quote_mint: Pubkey,
    base_vault: Pubkey,
    quote_vault: Pubkey,
    virtual_quote_reserves: i128,
}

impl PoolVaults {
    pub(crate) fn decode(pool_data: &[u8], base_mint: &Pubkey) -> Result<Self> {
        let (pool, virtual_quote_reserves) = decode_pool(pool_data)?;
        anyhow::ensure!(
            pool.base_mint == *base_mint,
            "pumpswap: pool is not for {base_mint}"
        );
        Ok(Self {
            quote_mint: pool.quote_mint,
            base_vault: pool.pool_base_token_account,
            quote_vault: pool.pool_quote_token_account,
            virtual_quote_reserves,
        })
    }

    pub(crate) fn addresses(&self) -> [Pubkey; 2] {
        [self.base_vault, self.quote_vault]
    }

    /// Quote base units per base-token base unit before the trade.
    pub(crate) fn spot_price(&self, base_balance: u64, quote_balance: u64) -> Result<f64> {
        anyhow::ensure!(base_balance > 0, "pumpswap: pool has no base reserves");
        let quote_reserves = effective_quote_reserves(quote_balance, self.virtual_quote_reserves)?;
        Ok(quote_reserves as f64 / base_balance as f64)
    }
}

#[derive(BorshDeserialize)]
#[allow(dead_code)]
struct GlobalConfigAccount {
    discriminator: u64,
    admin: Pubkey,
    lp_fee_basis_points: u64,
    protocol_fee_basis_points: u64,
    disable_flags: u8,
    protocol_fee_recipients: [Pubkey; 8],
    coin_creator_fee_basis_points: u64,
    admin_set_coin_creator_authority: Pubkey,
    whitelist_pda: Pubkey,
    reserved_fee_recipient: Pubkey,
    mayhem_mode_enabled: bool,
    reserved_fee_recipients: [Pubkey; 7],
    is_cashback_enabled: bool,
    buyback_fee_recipients: [Pubkey; 8],
}

#[derive(Clone, Copy)]
struct FeeSettings {
    standard_protocol_recipient: Pubkey,
    mayhem_protocol_recipient: Pubkey,
    buyback_recipient: Pubkey,
    lp_bps: u64,
    protocol_bps: u64,
    creator_bps: u64,
}

impl FeeSettings {
    fn protocol_recipient(self, is_mayhem: bool) -> Pubkey {
        if is_mayhem {
            self.mayhem_protocol_recipient
        } else {
            self.standard_protocol_recipient
        }
    }
}

struct PoolState {
    coin_creator: Pubkey,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    base_vault: Pubkey,
    quote_vault: Pubkey,
    base_reserves: u64,
    quote_reserves: u64,
    is_mayhem: bool,
    is_cashback: bool,

    base_token_program: Pubkey,
    quote_token_program: Pubkey,
}

struct BuyAmounts {
    quote_in: u64,
    min_base_out: u64,
}

pub struct PumpSwap {
    rpc: Arc<RpcClient>,
    fee_settings: OnceLock<FeeSettings>,
    sol_usdc_pool: Pubkey,
    sol_usdc_vaults: OnceLock<[Pubkey; 2]>,
}

impl PumpSwap {
    /// The SOL/USDC pool and its vaults. Vault addresses never change, so they're read once.
    pub(crate) async fn sol_usdc_price_accounts(&self) -> Result<[Pubkey; 3]> {
        let [usdc_vault, sol_vault] = match self.sol_usdc_vaults.get() {
            Some(vaults) => *vaults,
            None => {
                let account = self.rpc.get_account(&self.sol_usdc_pool).await?;
                let vaults = PoolVaults::decode(&account.data, &USDC_MINT)?.addresses();
                *self.sol_usdc_vaults.get_or_init(|| vaults)
            }
        };
        Ok([self.sol_usdc_pool, usdc_vault, sol_vault])
    }

    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            fee_settings: OnceLock::new(),
            sol_usdc_pool: DEFAULT_SOL_USDC_POOL,
            sol_usdc_vaults: OnceLock::new(),
        }
    }

    pub fn with_sol_usdc_pool(mut self, pool: Pubkey) -> Self {
        self.sol_usdc_pool = pool;
        self
    }

    pub async fn shared_lookup_addresses(&self) -> Result<Vec<Pubkey>> {
        let account = self.rpc.get_account(&Self::global_config_pda()).await?;
        let global: GlobalConfigAccount = decode_account(&account.data)?;
        let mut addresses = vec![
            PROGRAM_ID,
            FEE_PROGRAM_ID,
            Self::global_config_pda(),
            Self::event_authority_pda(),
            Self::global_volume_pda(),
            Self::fee_config_pda(),
        ];
        let recipients = global
            .protocol_fee_recipients
            .into_iter()
            .chain([global.reserved_fee_recipient])
            .chain(global.reserved_fee_recipients)
            .chain(global.buyback_fee_recipients);
        for recipient in recipients.filter(|key| *key != Pubkey::default()) {
            addresses.push(recipient);
            addresses.push(ata(&recipient, &WSOL, &TOKEN_PROGRAM));
        }
        Ok(addresses)
    }

    /// The token's graduated SOL pool.
    pub fn canonical_pool_pda(mint: &Pubkey) -> Pubkey {
        let pool_authority = pda(&[b"pool-authority", mint.as_ref()], &PUMPFUN_PROGRAM_ID);
        pda(
            &[
                b"pool",
                &CANONICAL_POOL_INDEX.to_le_bytes(),
                pool_authority.as_ref(),
                mint.as_ref(),
                WSOL.as_ref(),
            ],
            &PROGRAM_ID,
        )
    }

    /// `None` for tokens without a graduated SOL pool, including USDC-paired ones; use an
    /// aggregator for those. Pools created outside graduation are not found; pass them with
    /// `Trade::with_pool`.
    pub async fn find_market(&self, mint: &Pubkey) -> Result<Option<NativeMarket>> {
        let address = Self::canonical_pool_pda(mint);
        let response = self
            .rpc
            .get_account_with_commitment(&address, self.rpc.commitment())
            .await?;
        let Some(account) = response.value.filter(|account| account.owner == PROGRAM_ID) else {
            return Ok(None);
        };
        let (pool, _) = decode_pool(&account.data)?;
        let tradable = pool.base_mint == *mint && pool.quote_mint == WSOL;
        Ok(tradable.then_some(NativeMarket {
            source: QuoteSource::PumpSwap,
            pool: address,
        }))
    }

    fn global_config_pda() -> Pubkey {
        pda(&[b"global_config"], &PROGRAM_ID)
    }
    fn event_authority_pda() -> Pubkey {
        pda(&[b"__event_authority"], &PROGRAM_ID)
    }
    fn coin_creator_vault_authority_pda(coin_creator: &Pubkey) -> Pubkey {
        pda(&[b"creator_vault", coin_creator.as_ref()], &PROGRAM_ID)
    }
    fn global_volume_pda() -> Pubkey {
        pda(&[b"global_volume_accumulator"], &PROGRAM_ID)
    }
    fn user_volume_pda(user: &Pubkey) -> Pubkey {
        pda(&[b"user_volume_accumulator", user.as_ref()], &PROGRAM_ID)
    }
    fn fee_config_pda() -> Pubkey {
        pda(&[b"fee_config", PROGRAM_ID.as_ref()], &FEE_PROGRAM_ID)
    }
    fn pool_v2_pda(base_mint: &Pubkey) -> Pubkey {
        pda(&[b"pool-v2", base_mint.as_ref()], &PROGRAM_ID)
    }

    async fn fee_settings(&self) -> Result<FeeSettings> {
        if let Some(f) = self.fee_settings.get() {
            return Ok(*f);
        }
        let acc = self.rpc.get_account(&Self::global_config_pda()).await?;
        let gc: GlobalConfigAccount = decode_account(&acc.data)?;
        let f = FeeSettings {
            standard_protocol_recipient: gc.protocol_fee_recipients[0],
            mayhem_protocol_recipient: gc.reserved_fee_recipient,
            buyback_recipient: gc.buyback_fee_recipients[0],
            lp_bps: gc.lp_fee_basis_points,
            protocol_bps: gc.protocol_fee_basis_points,
            creator_bps: gc.coin_creator_fee_basis_points,
        };
        let _ = self.fee_settings.set(f);
        Ok(f)
    }

    async fn load_pool(&self, pool_address: &Pubkey, mint: &Pubkey) -> Result<PoolState> {
        let acc = self.rpc.get_account(pool_address).await?;
        let (pool, virtual_quote_reserves) = decode_pool(&acc.data)
            .map_err(|e| anyhow!("pumpswap: bad pool {pool_address}: {e}"))?;
        if pool.base_mint != *mint {
            return Err(anyhow!(
                "pumpswap: pool base mint {} != requested mint {mint}",
                pool.base_mint
            ));
        }

        let mints = self
            .rpc
            .get_multiple_accounts(&[pool.base_mint, pool.quote_mint])
            .await?;
        let base_token_program = mints[0]
            .as_ref()
            .ok_or_else(|| anyhow!("pumpswap: base mint not found"))?
            .owner;
        let quote_token_program = mints[1]
            .as_ref()
            .ok_or_else(|| anyhow!("pumpswap: quote mint not found"))?
            .owner;
        let (base_balance, quote_balance) = tokio::try_join!(
            self.rpc
                .get_token_account_balance(&pool.pool_base_token_account),
            self.rpc
                .get_token_account_balance(&pool.pool_quote_token_account),
        )?;
        Ok(PoolState {
            coin_creator: pool.coin_creator,
            base_mint: pool.base_mint,
            quote_mint: pool.quote_mint,
            base_vault: pool.pool_base_token_account,
            quote_vault: pool.pool_quote_token_account,
            base_reserves: base_balance.amount.parse()?,
            quote_reserves: effective_quote_reserves(
                quote_balance.amount.parse()?,
                virtual_quote_reserves,
            )?,
            is_mayhem: pool.is_mayhem_mode,
            is_cashback: pool.is_cashback_coin,
            base_token_program,
            quote_token_program,
        })
    }

    fn compute_quote(
        &self,
        pool: &PoolState,
        fees: &FeeSettings,
        side: Side,
        amount: u64,
        slippage_bps: u64,
    ) -> Quote {
        let creator_bps = if pool.coin_creator != Pubkey::default() {
            fees.creator_bps
        } else {
            0
        };
        match side {
            Side::Buy => {
                let total_bps = fees.lp_bps + fees.protocol_bps + creator_bps;
                let mut quote_into_pool =
                    (amount as u128 * 10_000 / (10_000 + total_bps as u128)) as u64;
                let fee = fee_floor(quote_into_pool, fees.lp_bps)
                    + fee_floor(quote_into_pool, fees.protocol_bps)
                    + fee_floor(quote_into_pool, creator_bps);
                if quote_into_pool + fee > amount {
                    quote_into_pool =
                        quote_into_pool.saturating_sub(quote_into_pool + fee - amount);
                }
                let expected_out = base_out_for_quote_in(
                    pool.quote_reserves,
                    pool.base_reserves,
                    quote_into_pool.saturating_sub(1),
                );

                Quote {
                    in_amount: amount,
                    usd_value: None,
                    application_fee: 0,
                    sponsorship_fee: 0,
                    expected_out,
                    min_out: slippage_down(expected_out, slippage_bps),
                    fee: amount.saturating_sub(quote_into_pool),
                }
            }
            Side::Sell => {
                let gross_quote_out =
                    quote_out_for_base_in(pool.quote_reserves, pool.base_reserves, amount);
                let fee = fee_floor(gross_quote_out, fees.lp_bps)
                    + fee_floor(gross_quote_out, fees.protocol_bps)
                    + fee_floor(gross_quote_out, creator_bps);
                let expected_out = gross_quote_out.saturating_sub(fee);
                Quote {
                    in_amount: amount,
                    usd_value: None,
                    application_fee: 0,
                    sponsorship_fee: 0,
                    expected_out,
                    min_out: slippage_down(expected_out, slippage_bps),
                    fee,
                }
            }
        }
    }

    fn named_accounts(
        &self,
        p: &Trade,
        pool_address: Pubkey,
        pool: &PoolState,
        fees: FeeSettings,
    ) -> Vec<AccountMeta> {
        let creator_vault_authority = Self::coin_creator_vault_authority_pda(&pool.coin_creator);
        let protocol_recipient = fees.protocol_recipient(pool.is_mayhem);
        vec![
            AccountMeta::new(pool_address, false),
            AccountMeta::new(p.wallet, true),
            AccountMeta::new_readonly(Self::global_config_pda(), false),
            AccountMeta::new_readonly(pool.base_mint, false),
            AccountMeta::new_readonly(pool.quote_mint, false),
            AccountMeta::new(
                ata(&p.wallet, &pool.base_mint, &pool.base_token_program),
                false,
            ),
            AccountMeta::new(
                ata(&p.wallet, &pool.quote_mint, &pool.quote_token_program),
                false,
            ),
            AccountMeta::new(pool.base_vault, false),
            AccountMeta::new(pool.quote_vault, false),
            AccountMeta::new_readonly(protocol_recipient, false),
            AccountMeta::new(
                ata(
                    &protocol_recipient,
                    &pool.quote_mint,
                    &pool.quote_token_program,
                ),
                false,
            ),
            AccountMeta::new_readonly(pool.base_token_program, false),
            AccountMeta::new_readonly(pool.quote_token_program, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(ATA_PROGRAM, false),
            AccountMeta::new_readonly(Self::event_authority_pda(), false),
            AccountMeta::new_readonly(PROGRAM_ID, false),
            AccountMeta::new(
                ata(
                    &creator_vault_authority,
                    &pool.quote_mint,
                    &pool.quote_token_program,
                ),
                false,
            ),
            AccountMeta::new_readonly(creator_vault_authority, false),
        ]
    }

    fn remaining_accounts(
        &self,
        p: &Trade,
        pool: &PoolState,
        fees: &FeeSettings,
        side: Side,
    ) -> Vec<AccountMeta> {
        let mut accounts = vec![];
        if pool.is_cashback {
            accounts.push(AccountMeta::new(
                ata(
                    &Self::user_volume_pda(&p.wallet),
                    &pool.quote_mint,
                    &pool.quote_token_program,
                ),
                false,
            ));
            if side == Side::Sell {
                accounts.push(AccountMeta::new(Self::user_volume_pda(&p.wallet), false));
            }
        }
        if pool.coin_creator != Pubkey::default() {
            accounts.push(AccountMeta::new_readonly(
                Self::pool_v2_pda(&pool.base_mint),
                false,
            ));
        }
        accounts.push(AccountMeta::new_readonly(fees.buyback_recipient, false));
        accounts.push(AccountMeta::new(
            ata(
                &fees.buyback_recipient,
                &pool.quote_mint,
                &pool.quote_token_program,
            ),
            false,
        ));
        accounts
    }

    fn swap_accounts(
        &self,
        p: &Trade,
        pool_address: Pubkey,
        pool: &PoolState,
        fees: FeeSettings,
    ) -> Vec<AccountMeta> {
        let mut accounts = self.named_accounts(p, pool_address, pool, fees);
        if p.side == Side::Buy {
            accounts.push(AccountMeta::new_readonly(Self::global_volume_pda(), false));
            accounts.push(AccountMeta::new(Self::user_volume_pda(&p.wallet), false));
        }
        accounts.push(AccountMeta::new_readonly(Self::fee_config_pda(), false));
        accounts.push(AccountMeta::new_readonly(FEE_PROGRAM_ID, false));
        accounts.extend(self.remaining_accounts(p, pool, &fees, p.side));
        accounts
    }

    fn buy_instructions(
        &self,
        p: &Trade,
        pool_address: Pubkey,
        pool: &PoolState,
        fees: FeeSettings,
        amounts: BuyAmounts,
    ) -> Vec<Instruction> {
        let mut data = Vec::with_capacity(25);
        data.extend_from_slice(&anchor_discriminator(BUY_EXACT_QUOTE_IN_IX));
        data.extend_from_slice(&amounts.quote_in.to_le_bytes());
        data.extend_from_slice(&amounts.min_base_out.to_le_bytes());
        data.push(TRACK_VOLUME);

        let user_quote_ata = ata(&p.wallet, &pool.quote_mint, &pool.quote_token_program);
        vec![
            create_ata_idempotent(
                &p.wallet,
                &p.wallet,
                &pool.base_mint,
                &pool.base_token_program,
            ),
            create_ata_idempotent(
                &p.wallet,
                &p.wallet,
                &pool.quote_mint,
                &pool.quote_token_program,
            ),
            system_transfer(&p.wallet, &user_quote_ata, amounts.quote_in),
            sync_native(&user_quote_ata),
            Instruction {
                program_id: PROGRAM_ID,
                accounts: self.swap_accounts(p, pool_address, pool, fees),
                data,
            },
            close_account(&user_quote_ata, &p.wallet, &p.wallet),
        ]
    }

    fn sell_instructions(
        &self,
        trade: &Trade,
        pool_address: Pubkey,
        pool: &PoolState,
        fees: FeeSettings,
        min_quote_out: u64,
    ) -> Vec<Instruction> {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&anchor_discriminator(SELL_IX));
        data.extend_from_slice(&trade.amount.to_le_bytes());
        data.extend_from_slice(&min_quote_out.to_le_bytes());
        let quote_ata = ata(&trade.wallet, &WSOL, &pool.quote_token_program);
        vec![
            create_ata_idempotent(
                &trade.wallet,
                &trade.wallet,
                &pool.quote_mint,
                &pool.quote_token_program,
            ),
            Instruction {
                program_id: PROGRAM_ID,
                accounts: self.swap_accounts(trade, pool_address, pool, fees),
                data,
            },
            close_account(&quote_ata, &trade.wallet, &trade.wallet),
        ]
    }

    /// Loads the trade's pool and fees, accepting only SOL-settled trades on SOL pools.
    async fn load_sol_pool(&self, trade: &Trade) -> Result<(Pubkey, PoolState, FeeSettings)> {
        anyhow::ensure!(trade.settlement == Settlement::Sol, SOL_ONLY);
        let pool_address = trade
            .pool
            .unwrap_or_else(|| Self::canonical_pool_pda(&trade.mint));
        let pool = self.load_pool(&pool_address, &trade.mint).await?;
        anyhow::ensure!(pool.quote_mint == WSOL, SOL_ONLY);
        Ok((pool_address, pool, self.fee_settings().await?))
    }
}

#[async_trait]
impl Dex for PumpSwap {
    fn name(&self) -> &'static str {
        "pumpswap"
    }

    async fn quote(&self, p: &Trade) -> Result<Quote> {
        let (_, pool, fees) = self.load_sol_pool(p).await?;
        Ok(self.compute_quote(&pool, &fees, p.side, p.amount, p.slippage_bps))
    }

    async fn prepare_swap(&self, p: &Trade) -> Result<PreparedSwap> {
        let (pool_address, pool, fees) = self.load_sol_pool(p).await?;
        let quote = self.compute_quote(&pool, &fees, p.side, p.amount, p.slippage_bps);
        let instructions = match p.side {
            Side::Buy => self.buy_instructions(
                p,
                pool_address,
                &pool,
                fees,
                BuyAmounts {
                    quote_in: p.amount,
                    min_base_out: quote.min_out,
                },
            ),
            Side::Sell => self.sell_instructions(p, pool_address, &pool, fees, quote.min_out),
        };
        Ok(PreparedSwap {
            venue: self.name(),
            quote,
            instructions,
            lookup_tables: vec![],
        })
    }
}
