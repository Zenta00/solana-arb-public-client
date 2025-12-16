# Solana validator fork + intra-slot arbitrage engine (Rust)

This repository is a **Solana/Agave validator codebase** with an embedded **intra-slot DEX arbitrage engine** written in Rust that performed really well in a highly competitive environment. This code is not intended to be built and used as it is deprecated, but can be used as an inspiration instead.


The interesting part (and what this README focuses on) is the end-to-end pipeline that lets us:
- **Detect pool state changes inside a slot** (not “after the fact” via RPC polling)
- **Update local AMM state immediately** and search for profitable cycles (1+ hops, multiple pool types)
- **Pick the best input size automatically** by treating profit as a 1D optimization problem (Brent)
- **Ship transactions reliably during high congestion** using pre-established TPU QUIC connections
- **Report outcomes to a local analytics dashboard** for feedback-driven fee/tip tuning

If you’re looking for the base validator docs, see [`docs/README.md`](docs/README.md) and [`docs/src/`](docs/src/).

## What’s actually novel here

### 1) Validator-side “hook” for pool account writes (intra-slot signal)

Instead of polling `getProgramAccounts`, we intercept **account updates as the validator processes transactions** and emit a lightweight `PoolNotification` when the account looks like a DEX pool or is explicitly tracked.

- **Account update hook**: [`geyser-plugin-manager/src/accounts_update_notifier.rs`](geyser-plugin-manager/src/accounts_update_notifier.rs#L35-L116)
  - [`AccountsUpdateNotifierImpl::notify_account_update`](geyser-plugin-manager/src/accounts_update_notifier.rs#L89-L116) decides if an updated account is relevant and sends a `PoolNotification`.
  - [`is_main_pool`](geyser-plugin-manager/src/accounts_update_notifier.rs#L35-L75) contains the current heuristics (by owner program id + account size) used to auto-detect pools and also to discover “associated accounts” to track (ex: token reserve accounts).

### 2) A Unix domain socket bridge (validator → local arbitrage process)

The validator runs a small streaming server that serializes pool notifications + transaction status information and sends it over a **Unix domain socket** to a local arbitrage process.

- **Server inside validator**: [`arbitrage/src/arbitrage_streamer.rs`](arbitrage/src/arbitrage_streamer.rs#L40-L487)
  - Socket path: [`/tmp/solana-arbitrage-streamer.sock`](arbitrage/src/arbitrage_streamer.rs#L40)
  - Serializes:
    - slot ticks (header `0x03`)
    - pool notifications (header `0xac`)
    - filtered transaction status batches (header `0x00`)
- **Client in arbitrage process**: [`arbitrage/src/arbitrage_client.rs`](arbitrage/src/arbitrage_client.rs#L54-L262)
  - [`read_message`](arbitrage/src/arbitrage_client.rs#L148-L157) + [`read_pool_notifications`](arbitrage/src/arbitrage_client.rs#L166-L200) decode the stream and push into internal channels.

#### Dynamic tracking (feedback loop)

We don’t want to track “everything”, so the arbitrage process can **add/remove tracked pubkeys** at runtime. It sends pubkey updates back over the same socket, and the validator updates an in-memory `HashSet`.

- **Arbitrage sends**: [`TrackedPubkeys`](arbitrage/src/arbitrage_client.rs#L29-L47) in [`arbitrage/src/arbitrage_client.rs`](arbitrage/src/arbitrage_client.rs#L29-L132)
- **Validator receives and updates tracked set**: [`arbitrage/src/arbitrage_streamer.rs`](arbitrage/src/arbitrage_streamer.rs#L266-L339)
- **Tracked set is shared with the validator-side hook**: [`geyser-plugin-manager/src/accounts_update_notifier.rs`](geyser-plugin-manager/src/accounts_update_notifier.rs#L77-L116)

This is how we keep the “hot set” of accounts small while still reacting intra-slot.

## Arbitrage workflow (end-to-end)

### Step A — Maintain local pool state

Incoming `PoolNotification`s update the local representation of each pool (reserves, ticks/bins, derived prices).

- **Pool state update + “price changed?” detection**: [`arbitrage/src/simulation/pool_impact.rs`](arbitrage/src/simulation/pool_impact.rs#L291-L410)
  - [`handle_amm_impact(...)`](arbitrage/src/simulation/pool_impact.rs#L291-L410) maps notifications to a concrete AMM type and calls `pool.update_with_balances(...)`.
- **Unified pool interface**: [`arbitrage/src/pools/mod.rs`](arbitrage/src/pools/mod.rs#L150-L242)
  - Every pool type implements a common `Pool` trait, including:
    - `get_amount_out_with_cu(...)`
    - `get_amount_in(...)`
    - `get_amount_in_for_target_price(...)`
    - `update_with_balances(...)`

This is what lets the router treat DLMM/CLMM/constant-product pools uniformly.

### Step B — When a pool price moves, search for cycles

When a pool moves enough to matter, we search for routes that can close back into the farm token (often WSOL) with profit after fees.

- **Route generation**: [`arbitrage/src/simulation/router.rs`](arbitrage/src/simulation/router.rs#L91-L372)
  - [`Router::new_routes_from_price_movement(...)`](arbitrage/src/simulation/router.rs#L91-L170) kicks off route discovery from a “moving” pool.
  - [`Router::explore_path(...)`](arbitrage/src/simulation/router.rs#L172-L302) is the recursive path explorer (multi-hop via `RoutingStep::Intermediate`).
  - [`MAX_STEP_COUNT`](arbitrage/src/simulation/router.rs#L35-L36) controls the maximum intermediate depth (tunable for speed vs coverage).

### Step C — Convert “find the best trade size” into optimization (Brent)

For each candidate route, we need the best `amount_in`. Doing per-pool-type closed-form math for every hop combination gets unmaintainable fast, so we instead treat the route simulation as a function:

\[
profit(amount\_in) = out(amount\_in) - amount\_in
\]

We maximize profit (or profit-per-CU) using a derivative-free 1D optimizer.

- **Brent optimization wrapper**: [`arbitrage/src/simulation/optimization.rs`](arbitrage/src/simulation/optimization.rs#L1-L53)
  - Uses [`argmin::solver::brent::BrentOpt`](arbitrage/src/simulation/optimization.rs#L1-L53) and defines the “cost” as `-profit_per_cu`.
- **Where it is applied**: [`arbitrage/src/simulation/router.rs`](arbitrage/src/simulation/router.rs#L304-L372)
  - [`Router::handle_route(...)`](arbitrage/src/simulation/router.rs#L304-L372) calls [`optimize_arbitrage_profit(...)`](arbitrage/src/simulation/optimization.rs#L28-L53) and then final-simulates at the chosen `amount_in`.

Background on Brent’s method: [Brent’s method](https://en.wikipedia.org/wiki/Brent%27s_method)

### Step D — Build and send the transaction

Once a route is selected and sized, we build the swap instruction bundle and send it immediately, with logic tuned for congestion.

- **Instruction / tx construction**: [`ArbitrageRoute::get_arbitrage_instruction(...)`](arbitrage/src/simulation/router.rs#L768-L840) in [`arbitrage/src/simulation/router.rs`](arbitrage/src/simulation/router.rs#L768-L840)
- **Sender orchestration + analytics hooks**: [`arbitrage/src/sender/transaction_sender.rs`](arbitrage/src/sender/transaction_sender.rs#L227-L482)
  - Reports each attempt to the local dashboard via `Analyzer`.

## The “TPU connection table” trick (congestion resilience)

During congestion, “connecting to TPU when you need it” can be too late. The reliable strategy here is:
- **Pre-establish QUICs** to upcoming leaders *before* they’re elected
- **Keep the connection hot** by periodically opening a unidirectional stream, writing a single byte, and finishing the stream (without dropping the QUIC connection)

In code:
- **Connection manager + keepalive refresh**: [`arbitrage/src/connexion_routine_service.rs`](arbitrage/src/connexion_routine_service.rs#L152-L261)
  - [`ConnectionHandler::refresh(...)`](arbitrage/src/connexion_routine_service.rs#L152-L261) does the “open stream → write one byte → finish” pattern.
- **Send tx over the hot connection**: [`NativeSender::send_native_transaction(...)`](arbitrage/src/sender/transaction_sender.rs#L968-L1008) in [`arbitrage/src/sender/transaction_sender.rs`](arbitrage/src/sender/transaction_sender.rs#L954-L1031)

## Analytics / feedback loop

Every opportunity is reported to a local service so you can tune tips/fees dynamically based on what actually lands.

- **HTTP reporting client**: [`arbitrage/src/analyzer/analyzer.rs`](arbitrage/src/analyzer/analyzer.rs#L16-L112) ([`report_opportunity`](arbitrage/src/analyzer/analyzer.rs#L37-L85), [`report_transaction_success`](arbitrage/src/analyzer/analyzer.rs#L87-L111))
- **Decision logic for endpoint (native vs jito) and fee strategy**: [`arbitrage/src/sender/decision_maker.rs`](arbitrage/src/sender/decision_maker.rs#L41-L100)

## Repository map (where to look)

- **Validator-side hook & streamer**
  - [`geyser-plugin-manager/src/accounts_update_notifier.rs`](geyser-plugin-manager/src/accounts_update_notifier.rs#L35-L116)
  - [`core/src/validator.rs`](core/src/validator.rs#L1727-L1733) (wires `ArbitrageStreamer::new(...)` into validator startup)
  - [`arbitrage/src/arbitrage_streamer.rs`](arbitrage/src/arbitrage_streamer.rs#L172-L487)
- **Arbitrage process**
  - Entry point: [`arbitrage/src/main.rs`](arbitrage/src/main.rs) → [`arbitrage/src/arbitrage_service.rs`](arbitrage/src/arbitrage_service.rs)
  - Socket client: [`arbitrage/src/arbitrage_client.rs`](arbitrage/src/arbitrage_client.rs#L54-L262)
  - Pool tracking & discovery: [`arbitrage/src/tokens/pools_manager.rs`](arbitrage/src/tokens/pools_manager.rs), [`arbitrage/src/tokens/pools_searcher.rs`](arbitrage/src/tokens/pools_searcher.rs)
  - Pool math implementations: [`arbitrage/src/pools/`](arbitrage/src/pools/) (DLMM/CLMM/CPMM/etc.)
  - State updates + impact detection: [`arbitrage/src/simulation/pool_impact.rs`](arbitrage/src/simulation/pool_impact.rs#L291-L410)
  - Routing + simulation: [`arbitrage/src/simulation/router.rs`](arbitrage/src/simulation/router.rs#L91-L372)
  - Brent optimization: [`arbitrage/src/simulation/optimization.rs`](arbitrage/src/simulation/optimization.rs#L1-L53)
  - Sending + analytics: [`arbitrage/src/sender/transaction_sender.rs`](arbitrage/src/sender/transaction_sender.rs), [`arbitrage/src/analyzer/`](arbitrage/src/analyzer/)


