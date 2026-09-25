# Trading Aggregator

Private Rust SDK, crate name `trading_aggregator`, with named **Solana** and
**Robinhood** clients in one codebase. Solana supports Pump.fun, PumpSwap, Jupiter,
DFlow, bloXroute and Relay, SOL/USDC settlement, application fees, and optional
sponsorship. Robinhood mainnet uses Relay and the trading wallet pays gas.

## Named chain clients

```rust,ignore
use std::sync::Arc;
use trading_aggregator::{TradingAggregator, robinhood, solana};

let sdk = TradingAggregator::new()
    .with_solana(solana::Client::new(
        Arc::new(solana::RpcClient::new("https://api.mainnet-beta.solana.com".into())),
    ))
    .with_robinhood(robinhood::Client::new(robinhood_rpc_url)?);

let wallet: robinhood::Address = "0x1111111111111111111111111111111111111111".parse()?;
let token: robinhood::Address = "0x5fc5360d0400a0fd4f2af552add042d716f1d168".parse()?;
let trade = robinhood::Trade::exact_input(
    wallet,
    robinhood::Currency::Native, // ETH on Robinhood
    robinhood::Currency::Token(token),
    robinhood::Amount::from(10_000_000_000_000_000u64), // 0.01 ETH in wei
    100, // 1% slippage
);
let quote = sdk.robinhood()?.quote(&trade).await?;
```

Use `sdk.solana()?.quote(&trade)` / `.swap(&trade, signer, submitter, priority_fee)`
for Solana and `sdk.robinhood()?.quote(&trade)` / `.swap(&trade, signer, submitter)` for Robinhood.
Each chain can also be used directly without the facade. Unconfigured chain access
returns an error. Existing Solana root exports such as `TradingClient` remain
available under the renamed crate; new integrations can use `solana::Client`.

Robinhood uses chain ID **4663**, as published in the [network configuration](https://docs.robinhood.com/chain/connecting/)
and Relay's `/chains` response. Trades use explicit input/output currencies and
256-bit integer amounts in base units. Native ETH uses `Currency::Native`; tokens
use their Robinhood contract address. The same-chain token pair must be supported
by Relay. Solana's SOL/USDC settlement restriction stays specific to its client.

### Robinhood execution and fees

Robinhood follows the Solana shape: a `Signer` you implement, plus a `Submitter`
you choose. The client's RPC URL is used to fill nonce, gas and EIP-1559 fees, check
the node's chain ID, and poll receipts. The SDK never holds a private key.

```rust,ignore
// Normal node:
let submitter = robinhood::RpcSubmitter::new(robinhood_rpc_url)?;
// Or bloXroute (`robinhood_tx`); transactions are also eligible for BackRunMe:
let submitter = robinhood::BloxrouteSubmitter::new(blox_auth_header)?
    .with_backrun_reward_address(reward_address); // optional

let result = sdk.robinhood()?.swap(&trade, &signer, &submitter).await?;
// Or review and execute the same prepared quote, without another quote request:
let prepared = sdk.robinhood()?.prepare_swap(&trade).await?;
let quote = prepared.quote();
let transactions = prepared.transactions();
let result = sdk.robinhood()?.execute_swap(prepared, &signer, &submitter).await?;
```

Implement `robinhood::Signer` to return EIP-2718 signed bytes for the
`UnsignedTransaction` it receives:

```rust,ignore
struct KmsSigner;

#[async_trait::async_trait]
impl robinhood::Signer for KmsSigner {
    async fn sign(
        &self,
        wallet: &robinhood::Address,
        tx: &robinhood::UnsignedTransaction,
    ) -> anyhow::Result<robinhood::Bytes> {
        // Turnkey-style KMS: sign the unsigned payload, return its signed transaction bytes.
        //   kms.sign_transaction(wallet, tx.encoded_for_signing()).await
        // Digest signer: sign `tx.signing_hash()` and let the SDK encode it.
        //   Ok(tx.encode_signed(&kms.sign_digest(wallet, tx.signing_hash()).await?))
        todo!()
    }
}
```

Before submitting, the SDK decodes the signed bytes and rejects them if any field
differs from the prepared transaction or the recovered signer is not the trading
wallet. It also rejects a submitter whose reported hash differs from the signed
transaction's hash. `BloxrouteSubmitter` posts to `https://api.blxrbdn.com` with
`node_validation` on, so nonce or sender errors come back instead of being silently
dropped; `with_url(url)?` overrides the endpoint. Node and submitter constructors
return a `Config` error for an invalid URL or auth header instead of failing later.

Gas: Relay's gas limit and fees are used when supplied. Otherwise the SDK estimates
gas (+20%) and uses alloy's EIP-1559 estimate from recent fee history for any fee
Relay left out. A legacy Relay `gasPrice` becomes an equal EIP-1559 fee cap
and tip. Each transaction is filled right before it is signed, so the swap is
estimated only after its approval is mined.

The SDK requests Relay once with permits and sponsorship disabled. It accepts
optional token approvals followed by one same-chain swap, validates chain,
wallet, currencies, amounts, slippage, router/proxy and approval targets, and caps
new allowances at the trade amount. It confirms each transaction before sending
the next. Solver deposits, signature flows and cross-chain execution are rejected.
Relay supplies the router calldata; these checks do not independently decode or
verify every nested router call. Signers should still apply their own signing policies.

Quotes expire locally after 60 seconds. Receipt confirmation waits up to 60 seconds
per transaction, configurable with `with_confirmation_timeout`. On failure or timeout,
`SwapError.submitted` preserves known transaction hashes; already-mined approvals
remain on-chain. There is no automatic resubmission, requote, provider comparison,
or fallback. Check pending hashes before starting a new swap. A submitter error can
be ambiguous after a broadcast, so the hash is recorded in `SwapError.submitted`
before each submit call; the transaction may still land.

```rust,ignore
let fee = robinhood::AppFee::new(claim_address, 100)?; // 1%; zero disables
let client = robinhood::Client::new(robinhood_rpc_url)?
    .with_relay_api_key(relay_api_key)
    .with_app_fee(fee);
```

Relay app fees accrue as claimable USDC. `Quote.application_fee` includes the
quoted charge's currency and amount because Relay can convert the fee from another
asset. Quotes already contain net output; no second SDK fee is appended. A separate
gas sponsor is neither required nor configured for Robinhood.

### Adding another chain

`evm::Client<C>` owns the shared Relay quote parser and transaction execution.
A new supported chain needs a named module implementing the sealed `Network`
definition (chain ID, name, verified Relay router and approval proxy), a `Client`
alias, and facade accessors. Provider code stays in `evm/relay`;
adding a provider later need not change the named
chain API. Ethereum and BNB are not enabled yet.

## Layout

```text
src/
  lib.rs                   Crate root; Solana names re-exported for existing imports
  trading_aggregator.rs    Named-chain facade: solana() and robinhood()
  error.rs                 TradeError, shared by both chains
  http.rs                  Shared provider HTTP client (timeouts, auth, error mapping)
  usd_value.rs             UsdValue: paid, received and loss %, shared by both chains

  aggregators/             Provider HTTP APIs only: requests and response types
    mod.rs                 Encoded Solana instruction type several providers return
    relay.rs               One Relay client for both chains
    jupiter.rs, dflow.rs, bloxroute.rs

  solana/
    mod.rs                 Solana public API
    client/
      mod.rs               TradingClient: quote, prepare, submit, swap
      routing.rs           Picks the one selected source and applies fees
      usd_value.rs         USD pricing: pool spot prices, else provider values
    executor.rs            Builds, sizes, signs and confirms the transaction
    submit.rs              RPC and bloXroute submitters
    types.rs               Trade, Quote, QuoteSource, Signer, Submitter
    sources/               Turn aggregator responses into Solana instructions
      mod.rs               Shared decoding and quote checks
      jupiter.rs, dflow.rs, bloxroute.rs
      relay/               Relay adapter (mod.rs) and its app fees (app_fee.rs)
    dexes/
      mod.rs, common.rs    Module root and shared instructions
      pumpfun/, pumpswap/  SOL-only adapters (mod.rs) with their IDLs
    sdk_fee.rs             Settlement-currency fees
    provider_fee.rs        Provider-collected fees (bloXroute, DFlow, Relay)
    gas_sponsor/
      mod.rs               Separate fee payer and USDC reimbursement policy
      cost.rs              Simulated cost policy
    lookup_table.rs        Shared ALT loading

  evm/
    mod.rs                 Generic EVM client for named chains
    robinhood.rs           Robinhood mainnet configuration and public API
    execution.rs           Fill, sign, verify, submit and confirm each transaction
    unsigned.rs            EIP-1559 signing payload and signed-bytes verification
    node.rs                EVM node calls via alloy-provider: nonce, gas, fees, receipts
    submit.rs              RPC and bloXroute EVM submitters
    types.rs               Trade, Quote, Signer, Submitter
    sources/relay/         Relay quote checks (mod.rs) and transaction validation (transactions.rs)
examples/
  create_shared_alt.rs    ALT creation utility (running it spends SOL)
```

This is a library crate. Cargo keeps `publish = false`; pushing to GitHub does
not publish the package to crates.io.

## Check the project

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

## Use the SDK

The sections below cover Solana source selection, settlement fees, sponsorship,
ALTs, and signing. See [named chain clients](#named-chain-clients) for Robinhood.

SDK fees are optional and bypassable. In default SDK collection mode, buy fees
use gross input; sell fees use quoted expected output, not minimum or actual proceeds. The sell fee is fixed
when preparing the swap and is subtracted from both expected and minimum output.
Quotes are rejected if the fees would leave no positive minimum output. A successful
simulation is not a guarantee that a future transaction will land.

Both `submit` and `swap` return fee amounts from the transaction being submitted,
not from an earlier preview quote:

```rust,ignore
let result = client.swap(&trade, signer, submitter, 5_000).await?;
let sdk_fee = result.application_fee;     // SOL lamports or USDC base units, per trade.settlement.
let sponsor_fee = result.sponsorship_fee; // USDC base units; finalized reimbursement + service fee.
```

With default SDK fee collection, these are the encoded charges, collected only
if execution succeeds. Pending does not mean paid; failed execution rolls them back. Both are zero when their
feature is disabled. They do not itemize venue fees or user-paid network fees,
tips and account rent; sponsor reimbursement can include sponsor-paid costs.

`swap` also reports `amount_received`: the output balance change, read at `confirmed`
commitment (the level `swap` waits for), whatever commitment your `RpcClient` uses.
It is `None` when the swap is not confirmed or a balance read fails, never a guess.
SOL-settled sell proceeds are net of the network fee and tip the wallet paid.
`token_balance(wallet, mint)` returns `0` when the wallet has no token account.

## Aggregator configuration

Aggregator URLs are built in; only Solana's RPC client is required at construction.
All adapters are available immediately, and only the selected source is called.

| Aggregator | Default base URL | Optional client override |
| --- | --- | --- |
| [Jupiter](https://developers.jup.ag/docs/swap/build/index.md) | `https://api.jup.ag` | `with_jupiter_url(url)` |
| [DFlow](https://pond.dflow.net/build/trading-api/imperative/quote) | `https://quote-api.dflow.net` | `with_dflow_url(url)` |
| bloXroute | `https://api.blox.ag` | `with_bloxroute_url(url)` |
| [Relay](https://docs.relay.link/references/api/get-quote-v2) | `https://api.relay.link` | `with_relay_url(url)` |

Configure credentials for the providers you use. DFlow and bloXroute production
endpoints require credentials. URL overrides preserve existing credentials, and
credential setters preserve the chosen URL.

```rust,ignore
let client = solana::Client::new(rpc)
    .with_jupiter_api_key(jupiter_api_key)
    .with_dflow_api_key(dflow_api_key)
    .with_bloxroute_auth_header(blox_auth_header)
    .with_relay_api_key(relay_api_key);

// Optional endpoint override; the SDK appends the provider's API path.
let client = client.with_bloxroute_url("https://api-dev.blox.ag");

let robinhood = robinhood::Client::new(robinhood_rpc_url)?
    .with_relay_api_key(robinhood_relay_key);
let robinhood = robinhood.with_relay_url(custom_relay_url);
```

Direct adapters also use defaults: `Jupiter::new()`, `Bloxroute::new()`,
`DFlow::new(rpc)` and `Relay::new(rpc)`. Each supports `with_base_url(url)` and
`with_api_key(key)` (bloXroute uses `with_auth_header(value)`).

## Native market or aggregator?

`find_native_market(mint)` checks on-chain whether a token trades on Pump.fun or
PumpSwap right now, so you can pick a native source or fall back to an aggregator:

```rust,ignore
let trade = Trade::buy(wallet, mint, lamports, 100, None);
let trade = match client.find_native_market(&mint).await? {
    Some(market) => trade.with_quote_source(market.source).with_pool(market.pool),
    None => trade.with_quote_source(QuoteSource::Relay),
};
```

It returns the live bonding curve first, then the SOL pool the token graduated into.
`None` means no tradable native market: the token never launched on Pump, graduated
to Raydium, or is paired with another currency such as USDC or PUMP. Pools created
outside graduation are not discovered. Pump.fun and PumpSwap trade **SOL only**: a
USDC-settled trade with a native source returns an error, so use an aggregator for
USDC.

## Choose exactly one source

The SDK contacts **only the source you select**. There is no price comparison,
provider racing, automatic fallback, or provider retry. An HTTP 429, timeout,
malformed response, native-pool failure, or execution failure is returned to the
caller. Preparation has one 15-second deadline, including provider ALT loading.
DFlow uses its quote and instruction endpoints; bloXroute and Relay each use one
build request. RPC account/ALT reads are separate from aggregator requests.

```rust,ignore
use trading_aggregator::{QuoteSource, Settlement, Trade, TradingClient};

let client = TradingClient::new(rpc)
    .with_bloxroute_auth_header(blox_auth_header)
    .with_quote_source(QuoteSource::Bloxroute);

// Buys spend SOL or USDC; sells receive SOL or USDC. Amounts are base units.
let buy = Trade::buy(wallet, mint, 10_000_000, 100, None)
    .with_settlement(Settlement::Usdc);
let prepared = client.prepare_swap(&buy).await?; // Only bloXroute.

let sell = Trade::sell(wallet, mint, token_amount, 100, None)
    .with_settlement(Settlement::Usdc)
    .with_quote_source(QuoteSource::Relay); // Overrides the client, only Relay.
let quote = client.quote(&sell).await?;

let custom = Trade::buy(wallet, mint, sol_lamports, 100, None)
    .with_quote_source(QuoteSource::PumpSwap)
    .with_pool(pool);
```

Sources are `Jupiter`, `DFlow`, `Bloxroute`, `Relay`, `PumpFun`, and `PumpSwap`.
All aggregator adapters have production URLs by default. Setting credentials or
overriding a URL does not select a source or contact it.
Precedence is trade `quote_source`, client `quote_source`, legacy `venue`, then
**Relay** when nothing is specified. Legacy `venue: Some(...)` still selects that native adapter if no explicit
source is set, but no longer falls back or compares routes. `quote_source_for(&trade)` reports the
selection without network access. Direct `Trade` struct literals now need
`quote_source: None`; constructor calls are unchanged.

`prepare_swap(...).venue` identifies the selected adapter. `quote`, `prepare_swap`,
`submit`, and `swap` each prepare a fresh route from that same selected source;
calling a preview and then `swap` therefore makes two preparations. An aggregator
can choose different underlying DEX paths on each call. Slippage protects each
fresh snapshot, not the earlier preview's absolute minimum.

## USD value

`quote.usd_value` shows what the user pays and gets back in dollars, on both chains:

```rust
if let Some(usd) = quote.usd_value {
    println!("pay ${:.2}, get ${:.2}, lose {:.2}%", usd.paid(), usd.received(), usd.loss_percent());
}
```

On Solana, values come from the first source that works:

1. **Pool spot prices**, when the Pump pool is known: the source is PumpFun or PumpSwap,
   or the trade carries `pool` (a Pump.fun curve or PumpSwap pool). USDC is $1, SOL is
   priced from the PumpSwap SOL/USDC pool, and the token from its pool's spot price.
   `loss_percent` is then exactly fees plus price impact. The reads run in parallel with
   route preparation: one RPC call for a Pump.fun curve, two for a PumpSwap pool.
2. **The provider's USD amounts**: Relay (`amountUsd`) and bloXroute (`inputValueUsd` /
   `outputValueUsd`), scaled to the final amounts so SDK and sponsorship fees count
   toward the loss. These include the provider's price error, so thin tokens can show
   a negative loss (an apparent gain).
3. Otherwise `None` (Jupiter and DFlow trades with no Pump pool).

Robinhood uses Relay's `amountUsd`. A failed pool read or an unparseable value is
`None`, never a quote error. Call `.without_usd_value()` on either client to skip USD
pricing and its pool reads; `usd_value` is then always `None`.

bloXroute uses `/v1/swap-instructions` with the raw `Authorization` header. The
public testing endpoint is `https://api-dev.blox.ag` (mainnet assets). Its routes
use about 60 inline accounts and no lookup tables, so the SDK builds them as
**V1 transactions** (`PreparedSwap.format == TransactionFormat::V1`, live on mainnet
since 2026-09-15): up to 64 accounts and 4,096 bytes. bloXroute's budget
configuration (compute-unit floor, loaded-account limit, heap) goes into the V1
message header instead of ComputeBudget instructions, and the caller's priority fee
is written there as total lamports. The executor adds simulation headroom. Every
other source builds V0 transactions with lookup tables. The aggregator configuration
is independent of `BloxrouteSubmitter`; either can be used without the other.

V1 transactions must be encoded with `wincode` (Solana's wire encoding): `bincode`
writes V1 signatures in the wrong place. A `Signer` signs `tx.message.serialize()`,
which is already correct for V1. Signers that send the whole transaction to a signing
service must encode it with `wincode` and the service must accept V1.

Relay uses [`POST /quote/v2`](https://docs.relay.link/references/api/get-quote-v2)
with an optional `x-api-key`, exact input and explicit slippage. It accepts only
one same-chain Solana `swap` transaction; solver deposits, signatures, cross-chain
and multi-step execution are rejected. DFlow uses `/quote` → `/swap-instructions`
with `x-api-key`; its public development endpoint is `https://dev-quote-api.dflow.net`.

## Settlement-side fees

The default `FeeCollection::Sdk` preserves native SOL/USDC fee transfers. To use
bloXroute or DFlow's input/output platform fee mechanism instead:

```rust,ignore
use trading_aggregator::{FeeCollection, SdkFee};
let fee = SdkFee::new(fee_wallet, 100)?
    .with_collection(FeeCollection::Provider);
let client = client.with_sdk_fee(fee);
```

For bloXroute and DFlow, provider collection charges `inputMint` on buys and
`outputMint` on sells, always in the settlement asset. The SDK creates the recipient's settlement-token ATA
and does **not** append its normal fee transfer or deduct the fee again. Provider
quotes already contain net output. SOL platform fees arrive as **WSOL** in the
recipient ATA; use the default SDK collection to receive native SOL. USDC fees
arrive as USDC. Provider collection supports bloXroute, DFlow, and Relay; other
sources return an error.

Relay API fees require a separate **EVM claim address**, even for Solana swaps:

```rust,ignore
let fee = SdkFee::new(solana_fee_wallet, 100)?
    .with_collection(FeeCollection::Provider)
    .with_relay_fee_recipient(&relay_evm_claim_address)?;
let client = client.with_sdk_fee(fee).with_quote_source(QuoteSource::Relay);
```

The SDK validates the claim address as nonzero `0x` plus 40 hex digits and sends
`appFees: [{ recipient: relay_evm_claim_address, fee: "100" }]`. The trading wallet,
swap recipient, and sponsor remain Solana addresses. Configuring the claim address
alone does not enable API fees; `FeeCollection::Provider` selects collection.

Relay supports input-only app fees on same-chain Solana swaps, so this mode accepts
**SOL/USDC buys**, including sponsored USDC buys. Sells return an error before any
API request; explicitly use `FeeCollection::Sdk` for SOL/USDC output fees on Relay
sells. Returned `fees.app` must match the requested amount and SOL/USDC input
currency. The SDK reports that input charge in `application_fee` and preserves
Relay's net output without adding a second fee transfer.

Relay converts accrued app fees into an offchain USDC balance claimable using the
EVM address; they are not paid directly to `solana_fee_wallet`. Withdrawal is a
separate Relay operation. See [Relay's app fee contract](https://docs.relay.link/features/app-fees).

`application_fee` reports the total quoted platform charge, including any provider
share, not the integrator's net revenue. bloXroute's supplied specification describes
an 80/20 integrator/provider split for platform fees; this opt-in does not request
additional positive-slippage fees. Provider execution/positive-slippage policies
still apply. For sponsored buys in provider mode, the sponsorship ceiling is
reserved first and provider bps apply to the remaining input. bloXroute/DFlow sell
fees follow the provider's output-fee mechanism; the reported amount is quote-time, while the
provider may collect a percentage of actual execution output. SDK transfer mode
continues to encode a fixed fee from the quoted expected sell output.

## Sponsored USDC trades (user needs no SOL)

Configure the sponsor once. It handles account funding, network fees, and automatic
USDC reimbursement of simulated net SOL expense plus a **1 USDC service fee**:

```rust,ignore
use trading_aggregator::{GasSponsor, QuoteSource, SdkFee, Settlement, Trade, TradingClient};

// sponsor_signer: Arc<dyn trading_aggregator::Signer>, backed by your wallet/KMS.
// price_source: Arc<dyn SolUsdcPriceSource>, your trusted backend SOL/USDC feed.
let sponsor = GasSponsor::new(sponsor_wallet, sponsor_signer, price_source)?;

let client = TradingClient::new(rpc)
    .with_dflow_api_key(dflow_api_key)
    .with_quote_source(QuoteSource::DFlow)
    .with_sdk_fee(SdkFee::new(fee_wallet, 100)?) // Optional additional 1% trading fee.
    .with_gas_sponsor(sponsor);

let trade = Trade::buy(user_wallet, token_mint, 10_000_000, 100, None)
    .with_settlement(Settlement::Usdc);
let quote = client.quote(&trade).await?;
// 10 USDC budget: 0.10 trading fee + 3 sponsorship reserve + 6.90 swapped.

let result = client.swap(&trade, user_signer, submitter, 5_000).await?;
// result.sponsorship_fee is the final simulated-cost + service charge encoded in the transaction.
```

The sponsor wallet receives reimbursement by default. There is no separate billing
opt-in, fixed-fee mode or free-sponsorship mode. Default limits are **0.01 SOL** net
sponsor expense and **3 USDC** total charge. Optional settings stay on the sponsor:

```rust,ignore
let sponsor = sponsor
    .with_fee_recipient(fee_wallet)?
    .with_limits(20_000_000, 5_000_000)? // 0.02 SOL expense / 5 USDC total charge.
    .with_service_fee_usdc(1_000_000)?;  // Default: 1 USDC; zero still recovers expenses.
```

The sponsor pays SOL network fees, account rent (including fee-recipient ATAs),
priority fees, and submitter tips. User and sponsor sign the same final transaction;
the user remains the token owner. Existing signer adapters work for both roles.
Keep the sponsor funded with SOL. No contract deployment or user SOL top-up is needed.

This client requires sponsorship for **every USDC-settled buy/sell**, regardless
of the user's SOL balance. Unsupported sources return an error. It is opt-in, not automatic balance detection. Use an unsponsored client
for ordinary USDC trades. SOL-settled trades on either client remain unsponsored.
Sponsored execution uses only the selected Jupiter, DFlow or Relay source;
Pump.fun and PumpSwap trade SOL only, so they are never sponsored. The user stays
trader and token owner. Only the selected source is requested. A sponsored failure returns an error;
there is no provider fallback or downgrade to unsponsored execution. bloXroute
sponsorship is unsupported: its swap instruction creates missing token accounts
itself, with the user as the only signer and rent payer, so a sponsor cannot take
over that rent from outside the instruction. Relay uses `depositFeePayer` and verified sponsor-paid
ATA setup; routes that still require a user SOL transfer are rejected. See the official
[Jupiter payer](https://developers.jup.ag/docs/swap/advanced/gasless) and
[DFlow sponsorship](https://pond.dflow.net/spot/trading/sponsored-swaps) contracts.

The sponsorship charge is **additional** to `with_sdk_fee`; omit `with_sdk_fee`
to charge only sponsor expenses plus the service fee. Buys reserve both
fees from the gross input; sells subtract them from guaranteed USDC proceeds.
Amounts must remain positive after fees. `Quote.application_fee` and
`Quote.sponsorship_fee` itemize the charges; outputs are already net of fees.
Selling does not require an existing USDC balance. In SDK collection mode, a sell
percentage is based on gross quoted expected output, before the sponsorship ceiling; the minimum
must cover both fees and leave positive proceeds.

The USDC charge transfers atomically with the swap. Failed on-chain execution
reverts swaps and USDC fees, **but the sponsor still pays network fees**.
USDC is not a guaranteed dollar.
Enforce authentication, rate limits, and spending/priority-fee caps in your backend;
users can close sponsored token accounts and make you fund their rent again.
Never expose the sponsor key or offer an unrestricted transaction-signing endpoint.
Build transactions server-side with trusted providers and authorize the requested
trade before allowing the sponsor signer to sign. No economic-abuse protection is
implemented by this SDK configuration.

`prepare_swap` remains unsigned and includes the fee ceiling. Use `submit`/`swap`
to finalize reimbursement and collect both signatures. Shared ALTs still apply, and the packet-size check counts
both signatures. The selected provider may return a different underlying route on a later swap.

### How automatic sponsor reimbursement works

Every sponsor charges simulation-estimated net SOL spending
(network/priority fees, tips, and account deposits minus SOL returned to the sponsor)
converted to USDC, plus a default **1 USDC service fee**. This policy applies to
native, Jupiter, DFlow and Relay routes. It does not charge swap principal or add another
percentage trading fee. `GasSponsor::new` requires the price source up front so a
sponsor cannot be configured without the conversion needed for reimbursement.

`SolUsdcPriceSource::sol_usdc_price()` returns `SolUsdcPrice` with
`usdc_units_per_sol` (six-decimal USDC units; 150 USDC/SOL = 150_000_000) and
`observed_at` (the feed's `SystemTime` observation timestamp). There is no built-in
oracle or hardcoded market price. Zero, future-dated or older-than-30-second rates
are rejected. The source has a five-second timeout. Keep it backend-controlled;
do not accept a user's rate. `sponsor.with_service_fee_usdc(amount)` changes only
the service fee. Even with zero service fee, expenses are still recovered.

`quote` / `prepare_swap` reserve the **full configured USDC ceiling**, not an exact
cost estimate. On buys, that ceiling and the SDK fee are removed from the gross
input before routing; unused reserved USDC remains in the wallet, not swapped.
Consequently the input must cover the ceiling even if the eventual charge is lower.
On sells, quoted outputs conservatively subtract the ceiling; the selected
route's final gas cost is calculated before signing.

`submit` / `swap` simulate the built transaction, read its sponsor pre/post SOL
balances from the same simulation, replace only the sponsorship transfer amount,
then simulate again before either signer is called. The priority fee, tips, rent
for both fee-recipient accounts and the second signature are included. The RPC must
return simulation `fee`, `preBalances` and `postBalances`; missing data, changed
cost, simulation errors, stale prices or either exceeded cap stop before signing.
No route is retried after these execution checks. `SwapResult.sponsorship_fee`
reports the amount encoded in the submitted transaction, collected only on success.

Example: a simulated 0.08 USDC expense results in a 1.08 USDC charge, even if the
quote reserved 3 USDC. A 1.20 USDC expense results in 2.20 USDC, subject to the caps.
Manual submission of `prepare_swap` instructions would charge the ceiling;
**use `submit` / `swap` for cost recovery**.

This is pre-signing, simulation-based billing, not exact post-execution accounting
or an on-chain spending cap. Account state and SOL/USDC prices can move before the
transaction lands. Persistent account deposits remain an expense to the sponsor,
even when the user may later close an account and reclaim its rent. On failure the
USDC transfer rolls back but network fees remain the sponsor's loss.

## Before pushing

Keep keypairs outside the repository, preferably in your existing Solana config
directory. `.env`, `.env.*`, project `config/`, `*-keypair.json`, `.pem`, `.key`,
build output, and `.DS_Store` are ignored. Ignore rules do not remove secrets from
past commits or already tracked files.

Review the exact changes before staging and pushing:

```sh
git status --short
git diff --check
git diff
# After selecting files to stage:
git diff --cached --stat
git diff --cached
```

Do not include wallet JSON, seed phrases, private keys, API keys, or credentialed
RPC URLs.
