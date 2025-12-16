# Solana validator fork + intra-slot arbitrage engine (Rust)

This repository is a **Solana/Agave validator codebase** with an embedded **intra-slot DEX arbitrage engine** written in Rust.

The interesting part (and what this README focuses on) is the end-to-end pipeline that lets us:
- **Detect pool state changes inside a slot** (not “after the fact” via RPC polling)
- **Update local AMM state immediately** and search for profitable cycles (1+ hops, multiple pool types)
- **Pick the best input size automatically** by treating profit as a 1D optimization problem (Brent)
- **Ship transactions reliably during high congestion** using pre-established TPU QUIC connections
- **Report outcomes to a local analytics dashboard** for feedback-driven fee/tip tuning

If you’re looking for the base validator docs, see `docs/README.md` and `docs/src/`.

## What’s actually novel here

### 1) Validator-side “hook” for pool account writes (intra-slot signal)

Instead of polling `getProgramAccounts`, we intercept **account updates as the validator processes transactions** and emit a lightweight `PoolNotification` when the account looks like a DEX pool or is explicitly tracked.

- **Account update hook**: `geyser-plugin-manager/src/accounts_update_notifier.rs`
  - `AccountsUpdateNotifierImpl::notify_account_update` decides if an updated account is relevant and sends a `PoolNotification`.
  - `is_main_pool` contains the current heuristics (by owner program id + account size) used to auto-detect pools and also to discover “associated accounts” to track (ex: token reserve accounts).

### 2) A Unix domain socket bridge (validator → local arbitrage process)

The validator runs a small streaming server that serializes pool notifications + transaction status information and sends it over a **Unix domain socket** to a local arbitrage process.

- **Server inside validator**: `arbitrage/src/arbitrage_streamer.rs`
  - Socket path: `/tmp/solana-arbitrage-streamer.sock`
  - Serializes:
    - slot ticks (header `0x03`)
    - pool notifications (header `0xac`)
    - filtered transaction status batches (header `0x00`)
- **Client in arbitrage process**: `arbitrage/src/arbitrage_client.rs`
  - `read_message` + `read_pool_notifications` decode the stream and push into internal channels.

#### Dynamic tracking (feedback loop)

We don’t want to track “everything”, so the arbitrage process can **add/remove tracked pubkeys** at runtime. It sends pubkey updates back over the same socket, and the validator updates an in-memory `HashSet`.

- **Arbitrage sends**: `arbitrage/src/arbitrage_client.rs` (`TrackedPubkeys`)
- **Validator receives and updates tracked set**: `arbitrage/src/arbitrage_streamer.rs`
- **Tracked set is shared with the validator-side hook**: `geyser-plugin-manager/src/accounts_update_notifier.rs`

This is how we keep the “hot set” of accounts small while still reacting intra-slot.

## Arbitrage workflow (end-to-end)

### Step A — Maintain local pool state

Incoming `PoolNotification`s update the local representation of each pool (reserves, ticks/bins, derived prices).

- **Pool state update + “price changed?” detection**: `arbitrage/src/simulation/pool_impact.rs`
  - `handle_amm_impact(...)` maps notifications to a concrete AMM type and calls `pool.update_with_balances(...)`.
- **Unified pool interface**: `arbitrage/src/pools/mod.rs`
  - Every pool type implements a common `Pool` trait, including:
    - `get_amount_out_with_cu(...)`
    - `get_amount_in(...)`
    - `get_amount_in_for_target_price(...)`
    - `update_with_balances(...)`

This is what lets the router treat DLMM/CLMM/constant-product pools uniformly.

### Step B — When a pool price moves, search for cycles

When a pool moves enough to matter, we search for routes that can close back into the farm token (often WSOL) with profit after fees.

- **Route generation**: `arbitrage/src/simulation/router.rs`
  - `Router::new_routes_from_price_movement(...)` kicks off route discovery from a “moving” pool.
  - `Router::explore_path(...)` is the recursive path explorer (multi-hop via `RoutingStep::Intermediate`).
  - `MAX_STEP_COUNT` controls the maximum intermediate depth (tunable for speed vs coverage).

### Step C — Convert “find the best trade size” into optimization (Brent)

For each candidate route, we need the best `amount_in`. Doing per-pool-type closed-form math for every hop combination gets unmaintainable fast, so we instead treat the route simulation as a function:

\[
profit(amount\_in) = out(amount\_in) - amount\_in
\]

We maximize profit (or profit-per-CU) using a derivative-free 1D optimizer.

- **Brent optimization wrapper**: `arbitrage/src/simulation/optimization.rs`
  - Uses `argmin::solver::brent::BrentOpt` and defines the “cost” as `-profit_per_cu`.
- **Where it is applied**: `arbitrage/src/simulation/router.rs`
  - `Router::handle_route(...)` calls `optimize_arbitrage_profit(...)` and then final-simulates at the chosen `amount_in`.

Background on Brent’s method: `https://en.wikipedia.org/wiki/Brent%27s_method`

### Step D — Build and send the transaction

Once a route is selected and sized, we build the swap instruction bundle and send it immediately, with logic tuned for congestion.

- **Instruction / tx construction**: `arbitrage/src/simulation/router.rs` (`ArbitrageRoute::get_arbitrage_instruction(...)`)
- **Sender orchestration + analytics hooks**: `arbitrage/src/sender/transaction_sender.rs`
  - Reports each attempt to the local dashboard via `Analyzer`.

## The “TPU connection table” trick (congestion resilience)

During congestion, “connecting to TPU when you need it” can be too late. The reliable strategy here is:
- **Pre-establish QUICs** to upcoming leaders *before* they’re elected
- **Keep the connection hot** by periodically opening a unidirectional stream, writing a single byte, and finishing the stream (without dropping the QUIC connection)

In code:
- **Connection manager + keepalive refresh**: `arbitrage/src/connexion_routine_service.rs`
  - `ConnexionRoutineService` tracks the next leaders, maintains a `LeaderConnexionCache`, and refreshes at a tight loop.
  - `ConnectionHandler::refresh(...)` does the “open stream → write one byte → finish” pattern.
- **Send tx over the hot connection**: `arbitrage/src/sender/transaction_sender.rs`
  - `NativeSender::send_native_transaction(...)` picks a pre-connected leader and sends the wire transaction over `open_uni()`.

## Analytics / feedback loop

Every opportunity is reported to a local service so you can tune tips/fees dynamically based on what actually lands.

- **HTTP reporting client**: `arbitrage/src/analyzer/analyzer.rs` (`report_opportunity`, `report_transaction_success`)
- **Decision logic for endpoint (native vs jito) and fee strategy**: `arbitrage/src/sender/decision_maker.rs`

## Repository map (where to look)

- **Validator-side hook & streamer**
  - `geyser-plugin-manager/src/accounts_update_notifier.rs`
  - `core/src/validator.rs` (wires `ArbitrageStreamer::new(...)` into validator startup)
  - `arbitrage/src/arbitrage_streamer.rs`
- **Arbitrage process**
  - Entry point: `arbitrage/src/main.rs` → `arbitrage/src/arbitrage_service.rs`
  - Socket client: `arbitrage/src/arbitrage_client.rs`
  - Pool tracking & discovery: `arbitrage/src/tokens/pools_manager.rs`, `arbitrage/src/tokens/pools_searcher.rs`
  - Pool math implementations: `arbitrage/src/pools/` (DLMM/CLMM/CPMM/etc.)
  - State updates + impact detection: `arbitrage/src/simulation/pool_impact.rs`
  - Routing + simulation: `arbitrage/src/simulation/router.rs`
  - Brent optimization: `arbitrage/src/simulation/optimization.rs`
  - Sending + analytics: `arbitrage/src/sender/transaction_sender.rs`, `arbitrage/src/analyzer/`


