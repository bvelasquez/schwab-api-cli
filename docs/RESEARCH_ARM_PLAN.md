# Research-paper arm — plan (Public.com, paper only)

Status: **Phases 1 and 3 landed (paper loop is live-capable); Phase 2 offline eval and Phase 4 live are not built.** This document is the working plan.

Decisions locked: forecast target (§0.1) and web-search provider (§0.2).

Goal: one interval-driven agent that reads live Public.com market data plus configurable web/news sources, produces a bounded forecast and a bounded plan patch, and paper-trades the result — with a **structural hard gate** that makes a real order impossible. No Schwab accounts. No live money. Theory must clear a promotion gate before live is even a separate conversation.

> Public.com has no sandbox (`Capabilities::public().paper == false`). "Paper" here means **real market data, locally simulated fills, zero order requests** — not a broker sandbox.

---

## Implementation status

**Landed — Phase 1 safety spine and Phase 3 paper loop.**

| Piece | Location | Plan ref |
|---|---|---|
| Arm config + fail-closed validation | `crates/research-core/src/config.rs` | §5, App. C |
| Startup self-test | `crates/research-core/src/gate.rs` | §4, App. B |
| `PaperOnlyBroker` + tripwire | `crates/research-core/src/paper_broker.rs` | §4 L2/L5, App. A |
| Forecast contract | `crates/research-core/src/forecast.rs` | §0.1, App. D |
| Bounded patch engine (allowlist + rails) | `crates/research-core/src/patch.rs` | §5 |
| OpenRouter `web_search` client | `crates/research-core/src/web_search.rs` | §0.2, §6 |
| Read-only HTTP mode (trading writes refused) | `crates/broker-public/src/client.rs` | §4 L3 |
| Paper book: sizing, fills, marks, exits, kill switch | `crates/research-core/src/book.rs` | §7 |
| RSI/ATR over point-in-time daily bars | `crates/research-core/src/indicators.rs` | §7 |
| Versioned playbook overlay + rollback | `crates/research-core/src/playbook.rs` | §5 |
| Forward-return scoring + rank IC + stage-B gate | `crates/research-core/src/scoring.rs` | §8 |
| `research validate` / `tick` / `run` / `rollback` | `crates/agent-trader-cli/src/research.rs` | §3, §9 |
| Research tab + research paper book on the dashboard | `crates/agent-trader-cli/src/{dashboard.rs,dashboard.html}` | §9 |
| Loop / timer unit examples | `deploy/systemd/user/agent-trader-research.{service,timer}.example` | §9 |
| Example arm | `rules/research/example-megacaps.yaml` | App. C |

Tests: 81 in `research-core`, 2 in `broker-public`, 40 in `agent-trader-cli`, all green. `--trust`/`--yes` are rejected by the research path (L1).

**Not built yet:** configured-feed fetcher (§6 channel 1 — `research.feeds` is parsed but not fetched; web search is the only context channel), offline IC evaluation on `rules/lab-data/` (Phase 2), promotion-gate wiring to `rules/analysis/promotion-gate.json` (§8 stage D operator sign-off), and live (Phase 4, a separate artifact).

### Phase 3 as built

`agent-trader research run --rules rules/research/<arm>.yaml --interval-seconds 1800`
(optionally `--apply-patches`) drives one pass:

1. gate → read-only Public quotes → daily bars per symbol + benchmark → session state;
2. playbook = active overlay if any, else the arm YAML;
3. load the arm's paper book, mark positions, run exits (stop, target, time, thesis,
   ATR trailing ratchet), apply the daily-loss kill switch, then size entries from
   `equity / max_open_positions` for `up` signals at or above `min_confidence`
   inside the RSI band, with an entry cooldown after a stop/thesis exit;
4. LLM + web-search forecast (prompt includes quotes, positions, playbook and the
   scorecard so far);
5. version every accepted patch; activate only with `--apply-patches`. The active
   overlay is the full `Mutable` tree, so the agent can adapt exits, entry
   selectivity, sizing and hold time — not just stops;
6. score pending forecasts whose forward window has elapsed, append to
   `scores-<arm>.jsonl`, recompute the stage-B scorecard;
7. write the book to `rules/research/agent-trader-research-<arm>.json` (dashboard)
   plus `state/paper-research/`, journal the tick, re-check the tripwire.

Deviations from the original plan text, all deliberate:

- **Overlay store, not a rewritten YAML copy.** Versions live in
  `rules/research/<file>.versions/<ts>.json` with an `active.json` pointer. The
  arm YAML keeps its comments and operator edits, rollback is deleting the
  pointer, and only the `playbook` subtree is ever read back, so an overlay can
  never change account, mode, gate, universe or credentials.
- **`max_quote_age_seconds` is not enforced.** Public's quote payload carries no
  exchange timestamp, so a staleness test would be theatre: quotes are fetched at
  the top of a tick and used immediately. The field stays in the schema for
  backends that do expose a quote timestamp. The guard that *does* matter is
  point-in-time: indicators see only bars strictly before the decision date.
- **Long only.** The plan already excludes shorts; `down`/`flat`/missing signals
  close a held name ("thesis") instead of opening a short.
- **Daily buckets are US Eastern exchange dates.** Bar dates are exchange dates,
  so halts, cooldowns and the pending-forecast decision date all use the ET
  calendar (`indicators::exchange_date`), not the UTC date. A 21:00 ET tick is
  already the next day in UTC.

---

## 0. Decisions (locked)

### 0.1 Forecast target — forward 5-day excess return sign vs SPY, scored by cross-sectional rank IC

Primary target: **`forward_5d_excess_return_sign`** — for each symbol, the sign of its forward 5-trading-day return minus SPY over the same window.

Why this target:
- Benchmark-relative removes market beta, so the signal is judged on selection, not on being long a rising market.
- 5 trading days matches the decay window of news-driven drift and is a clean, scoreable horizon.
- The pass/fail metric is **cross-sectional Spearman rank IC**, which is exactly what `rules/analysis/llm-ic-eval.json` already measures: `mean_daily_ic >= 0.03`, day-clustered `t >= 2`, incremental vs RSI > 0, `>= 20` days.
- `expected_return_pct` and `confidence` are still emitted, but only for sizing/ranking. They are **not** the pass/fail target.

Secondary (diagnostic only, never a gate): volatility regime (`risk_on` / `risk_off` / `chop`) to modulate exposure. It does not claim return prediction.

### 0.2 Web search — OpenRouter `openrouter:web_search` server tool, engine `auto`

Yes, OpenRouter supports web search. Two mechanisms exist:

| Mechanism | Behavior | Fit here |
|---|---|---|
| `plugins: [{ "id": "web" }]` / `:online` suffix | Always runs **once per request**; Exa by default for non-native models | Simple grounding; not iterative |
| **`openrouter:web_search` server tool** (beta) | Model decides **when and how often** to search; may search **multiple times in one request**; supports `max_results`, `max_total_results`, `search_context_size`, `allowed_domains`, `excluded_domains`, `engine` | **Chosen** — this is the iterative "multiple searchable web criteria" behavior |

Configuration:

```json
{ "type": "openrouter:web_search",
  "parameters": {
    "engine": "auto",
    "max_results": 5,
    "max_total_results": 20,
    "search_context_size": "medium",
    "allowed_domains": ["reuters.com", "federalreserve.gov", "sec.gov", "bls.gov"]
  } }
```

- `engine: auto` uses native provider search when available and falls back to Exa — best quality/economy balance. Configurable per arm.
- **No new vendor.** Single `OPENROUTER_API_KEY`. The existing frozen stack already calls OpenRouter (`perplexity/sonar`), so this is the same bill.
- **Model split** (env-overridable):
  - search + synthesis turns: `google/gemini-2.5-flash` (cheap tokens, reliable structured JSON);
  - deep news-synthesis turns: `perplexity/sonar` (native search).
- **Cost controls**: `max_total_results` caps search spend per request; `max_queries_per_tick` caps turns per tick; cache results per tick and journal token + search spend.

Reference pricing (verify at build; OpenRouter pricing changes):

| Item | Cost |
|---|---|
| Exa engine (default fallback) | $0.007/request incl. 10 results, +$0.001/extra |
| Parallel engine (turbo/fast) | $0.001/request — cheapest bulk |
| Perplexity engine | $0.005/request |
| Native | provider passthrough (e.g. sonar `web_search` $0.005; Gemini grounding $0.014) |
| `perplexity/sonar` tokens | $1/M in, $1/M out |
| `google/gemini-2.5-flash` tokens | $0.30/M in, $2.50/M out |
| `openai/gpt-5-mini` tokens | $0.25/M in, $2/M out |

Rough per-turn estimate with `gemini-2.5-flash` + `engine: auto` (≈20k in / 3k out + one search): **≈ $0.01–0.02/turn**. Hourly during a 6.5h session over a 5-day week ≈ **< $1/week/arm**. This keeps Phase 1–3 cheap enough to run continuously.

---

## 1. Scope

### In scope
- A new `agent-trader research` subcommand (and a dedicated paper binary, see §4).
- Read-only Public.com market data + account data.
- Configurable web/news sources + an iterative `web_search` tool.
- LLM synthesis → forecast + bounded patch.
- Local paper ledger, paper fills, scorecard.
- Offline prediction evaluation on `rules/lab-data/`.
- A promotion gate that gates live, not a flag.

### Out of scope (explicitly)
- Any live order on any broker. No `place`, `cancel`, or `replace` reachable.
- Any Schwab account, credential, or backend.
- Options, shorts, margin, or complex orders in the paper arm.
- Writing `safety.json`, credentials, or the gate itself.
- Auto-promotion to live. Live is a separate artifact and a separate decision.

---

## 2. Feasibility map (existing primitives)

| Need | Exists today | Where |
|---|---|---|
| Public broker (read + order) | accounts, portfolio, orders, quotes, option expirations/chain/greeks, strategy quote, historic bars | `crates/broker-public/` |
| Broker-neutral traits | `Broker`, `MarketData`, `BrokerRegistry`, `OrderIntent` | `crates/broker-core/src/{traits,registry,intent,market}.rs` |
| Interval loop | tick loop, session modes, state/journal, hot reload | `crates/schwab-trader/src/agent/{daemon,runner,schedule,state}.rs` (frozen) |
| Cron workflows | 5-field cron, `active_between`, slot dedup, fetch/check/order steps | `crates/agent-trader-swing/src/{workflow,schedule}.rs` |
| Configurable web feeds | URL/API/RSS feeds with env-backed auth, phase routing | `crates/schwab-trader/src/sources.rs` (frozen) |
| Web-grounded LLM | `web_model: perplexity/sonar`, `web_insights` | `crates/schwab-trader/src/agent/llm.rs` (frozen) |
| Bounded plan patching | `apply_rule_patches` + allowlist + safety rails + `adaptation_allowed` | `crates/schwab-trader/src/learn.rs` (frozen) |
| Prediction eval | backtest lab, IC/t-stat eval, OOS + 15 bps checks | `crates/schwab-trader/src/backtest/`, `rules/analysis/llm-ic-eval.json`, `rules/analysis/swing-lab.json` |
| Promotion gate | pass criteria + operator note | `rules/analysis/promotion-gate.json` |
| Paper ledger | simulated fills/ledger | `crates/agent-trader-swing/src/{paper,workflow_paper}.rs` |
| Safety | `SafetyGate`, account lock, legacy-pid guard, license ack | `crates/agent-trader-cli/src/{safety,guard}.rs`, `crates/broker-public` |

### Gaps to build
1. `PaperOnlyBroker` decorator + paper registry (no Schwab).
2. Read-only mode in the Public HTTP client.
3. `web_search` tool (iterative queries, ranking, cache, citations).
4. Public-backed market-context builder.
5. Forecast schema + validation.
6. Versioned plan store with diff/rollback.
7. Startup self-test + runtime tripwire.
8. Shared `research-core` crate (frozen `schwab-trader` must not be imported — see `docs/AGENT_TRADER_FREEZE.md`).

---

## 3. Architecture

```
agent-trader research tick --rules rules/research/<arm>.yaml
        │
        ├─ 1. GATHER
        │     Public market data (quotes, bars, chain, portfolio)
        │     + configured feeds (research-core::feeds)
        │     + web_search tool (iterative, cached, cited)
        │
        ├─ 2. SYNTHESIZE (LLM)
        │     context -> forecast.json + plan_patch
        │
        ├─ 3. VALIDATE
        │     schema -> patch allowlist + safety rails -> SafetyGate
        │
        ├─ 4. APPLY
        │     versioned rules/workflow copy (diff + rollback)
        │
        ├─ 5. EXECUTE (paper)
        │     PaperOnlyBroker -> simulated fill -> paper ledger
        │
        └─ 6. JOURNAL + SCORE
              inputs hash -> forecast -> patch -> fills -> realized outcome
```

One new crate, `crates/research-core`, holds: `feeds`, `web_search`, `forecast`, `patch`, `paper_broker`, `gate`. `agent-trader-cli` gains a `research` module that wires it. `broker-public` gains read-only mode. Frozen crates are **read for reference only**; logic is copied, not depended on.

---

## 4. Hard gate (five independent layers)

Any one failing still prevents an order. All five required. The gate is **structural**, never a runtime flag.

### L1 — Command surface
- New `agent-trader research` subcommand. Its clap struct **has no `--trust` and no `--yes` fields** and no live branch.
- `TradingFlags` is a hardcoded constant in this path: `{ trust: false, yes: false, simulate: true, dry_run: false }`.
- There is no argument that turns trading on.
- Touchpoint: `crates/agent-trader-cli/src/main.rs` + new `src/research.rs`.

### L2 — Broker graph
- `build_paper_registry()` constructs **only** `broker_public`, wrapped in `PaperOnlyBroker`.
- The dedicated paper binary feature-gates `broker-schwab` **out** of its dependency graph, so the Schwab code cannot be linked in.
- `PaperOnlyBroker` implements `Broker`; `place`, `cancel`, `replace` return `Err`. Reads delegate.
- Touchpoints: `crates/research-core/src/paper_broker.rs`, new `build_paper_registry()` in `crates/agent-trader-cli/src/registry.rs`, `crates/agent-trader-cli/Cargo.toml` features.
- Do **not** call the existing `build_registry()` — it inserts Schwab whenever `SCHWAB_APP_KEY` is set.

### L3 — HTTP boundary
- `broker-public` client gains a `ReadOnly` mode that rejects any non-GET to `/userapigateway/trading/*` **before** the request is sent. Market-data GETs pass.
- Catches any accidental direct client use, bypassing L2.
- Touchpoint: `crates/broker-public/src/client.rs` (`send`).

### L4 — Account + config
- Account must parse to `AccountRef { broker: Public }`.
- **Bare ids are rejected.** `parse_account_address()` maps a bare string to Schwab (`crates/broker-core/src/account.rs`); the research path must refuse: "research arm requires `public:<id>`".
- Rules file `mode: paper` is mandatory; any other value refuses start.
- `adaptation.live_auto_apply: true` refuses start.
- `public_license_ack` must remain `false` (it is only needed for real Public trading; its absence is a tripwire).
- Touchpoint: `crates/research-core/src/gate.rs`.

### L5 — Runtime tripwire
- If `place` is ever reached, `PaperOnlyBroker` writes `unsafe_attempt.json`, sends an alert (if configured), and the process exits non-zero.
- On shutdown, assert `place_attempts == 0`.
- Any `unsafe_attempt.json` present = stop the arm and investigate before restart.

### Startup self-test (fail-closed)
On boot, assert all of:
1. registry contains no Schwab backend;
2. the Public broker is wrapped in `PaperOnlyBroker`;
3. account broker == `Public` and id is non-empty and not a bare hash;
4. rules `mode == paper`;
5. `adaptation.live_auto_apply == false`;
6. `SafetyGate.public_license_ack == false`;
7. `PUBLIC_PERSONAL_SECRET` is unset (warn hard if set).

On success, journal `paper_gate_ok` with hashes of binary + rules + gate config. Any failure → exit 1. This record is the audit proof that test mode was actually on.

### Layer summary

| # | Layer | Mechanism | Touchpoint | Test |
|---|---|---|---|---|
| L1 | Command | no trust/yes fields; hardcoded paper flags | `main.rs`, `research.rs` | compile-time + arg-struct test |
| L2 | Broker | `PaperOnlyBroker`; Schwab feature-gated out | `research-core`, `Cargo.toml` | registry set == {Public}; `place` -> Err |
| L3 | HTTP | non-GET to trading paths rejected pre-send | `broker-public/src/client.rs` | unit test, zero network |
| L4 | Account/config | reject bare hash, non-Public, non-paper mode | `research-core/src/gate.rs` | table tests |
| L5 | Runtime | tripwire file + exit + attempt counter | `research-core/src/paper_broker.rs` | integration test |

---

## 5. Mutated artifact

The agent mutates a **research rules file**, not static trade plans. Static `plans/*.yaml` stay human/LLM-authored and unchanged.

- Mutable: `rules/research/<arm>.yaml` (a workflow-style or rules-style file), specifically the `Mutable` tree: `playbook.*` (exits + RSI entry band), `paper.max_open_positions`, `paper.max_hold_days`, `forecast.min_confidence`. Risk controls, the cost model, the baseline equity, the scoring contract, account/mode/gate/universe and the allowlist itself are **never** patchable.
- Every tick that proposes a patch writes a **new version** to `rules/research/<arm>.versions/<ts>.yaml` plus a diff and a rollback pointer.
- The active file is only replaced after schema + allowlist + rails + self-test pass.
- Patch format follows the existing engine: `{ "path": "<dotted>", "value": <scalar|range>, "reason": "<cite sources + ids>" }`.
- Allowlist + rails copied from `crates/schwab-trader/src/learn.rs`:
  - allowlist: `playbook.exit.profit_target_pct`, `playbook.exit.stop_loss_pct`, `playbook.exit.trailing.trail_atr_multiple`, `playbook.exit.trailing.activate_after_profit_pct`, `playbook.entry.rsi_14_range`.
  - rails clamp (e.g. `profit_target_pct` 2.0–20.0, `stop_loss_pct` 1.5–10.0) and optional `max_delta_per_change`.
  - `immutable_fields` can never be patched.
- The agent may **never** patch: account, broker, mode, gate settings, credentials, `safety.json`, source allowlist, or the allowlist/rails themselves.

---

## 6. Web research layer

Two channels:

1. **Configured feeds** (ported from `sources.rs`): `url` / `api` / `rss`, phase-routed, env-backed auth (`token_env`), byte/timeout caps, fetched per tick and attached as `source_feeds` with citations. Good for stable primary sources (Fed, SEC, BLS, earnings calendar).
2. **`web_search` tool** (new) via the **OpenRouter `openrouter:web_search` server tool** — see §0.2. The model iterates searches within a turn and may run several per tick. Engine, domains, and result caps are arm config. Implemented in `research-core::web_search` as a thin OpenRouter client call that passes the tool and parses `url_citation` annotations.

Query discipline:
- The agent proposes a **bounded query plan** (≤ `max_queries_per_tick`), each query tied to a criterion (macro, sector, symbol catalyst, risk).
- Domain allowlist enforced server-side via `allowed_domains`; the agent cannot add sources at runtime.
- Every claim carries a source URL + timestamp. Results are cached per tick.
- Search spend (requests, results, tokens) is journaled every tick.

Rules for research integrity:
- Every claim carries a source id + timestamp.
- Cache results per tick to control cost; record query count and token spend.
- Sources are an allowlist in the arm config; the agent cannot add sources at runtime.
- No source content is treated as an instruction (prompt-injection guard): feed text is data, and the system prompt says so.

---

## 7. Paper fill model

Results are only as good as this model. It must be explicit and logged.

As built (`crates/research-core/src/book.rs`):

- **Fill rule**: a market fill against the quote fetched at the top of the tick: ask for buys, bid for sells, last/mid fallback. Skips the symbol when no usable price exists. `max_quote_age_seconds` is not enforced (Public quotes carry no timestamp; see "Phase 3 as built").
- **Costs**: `slippage_bps` (default 5) applied against the book on both sides, plus the real half spread (entry marks to the last immediately). Public equity commission = $0.
- **Marks**: Public quotes each tick; the paper book is marked to market every pass.
- **Sizing**: `equity / max_open_positions` per slot, capped by cash; whole shares; never live buying power.
- **Exits**: stop, profit target, ATR trailing ratchet (only up), time stop at `max_hold_days` trading days, and a "thesis" exit when the signal that opened the trade is gone, plus a one-trading-day re-entry cooldown after a stop/thesis exit.
- **Kill switch**: realised loss at or below `-max_daily_loss_usd` halts new entries for the rest of the UTC day; the halt persists in the book across restarts.
- **Point-in-time discipline**: RSI/ATR see only bars strictly before the decision date; scoring uses only closes after it. No future bars in a decision.
- **Session gate**: local US equity calendar (Public has no hours endpoint). Closed session blocks entries; marks and exits still run.
- **Ledger**: `rules/research/agent-trader-research-<arm>.json` for the dashboard plus a copy in `.../agent-trader/paper-research/`; never reconciled against live positions.

---

## 8. Promotion gate (money protection)

Real money is untouched through this entire plan. Promotion to live is a **separate artifact** and a separate operator decision, never a flag flip on the paper arm.

| Stage | Requirement |
|---|---|
| A — paper duration | ≥ 40 trading days, ≥ N trades, no gate violations, no `unsafe_attempt` |
| B — signal quality | clears `rules/analysis/llm-ic-eval.json`: mean IC ≥ 0.03, day-clustered t ≥ 2, incremental vs RSI > 0, ≥ 20 days |
| C — robustness | OOS positive, survives 15 bps (reuse `swing-lab.json` check shape) |
| D — operator | explicit sign-off recorded in `rules/analysis/promotion-gate.json`; live arm is a different binary/config |

`promotion-gate.json` already states: "A pass is not a production change. Operator OK is required. Real money is a separate decision." Keep that contract.

---

## 9. Phased roadmap

| Phase | Deliverable | Exit criteria |
|---|---|---|
| **0 — Freeze interfaces** | Written spec: mutable artifact, forecast target/horizon, source allowlist, interval, fill model. | This document accepted; no code. |
| **1 — Read-only research loop** | `agent-trader research tick` gathers Public data + `web_search`, writes the forecast + proposed patch to the journal. **No apply, no `place`.** — **landed** | Reproducible run; citations captured; cost/tick measured. |
| **2 — Offline prediction gate** | Evaluate the forecast on `rules/lab-data/` with point-in-time discipline. | Clears stage B — or a documented "signal is noise". Both are valid results. |
| **3 — Paper apply loop (destination)** | Bounded patches → paper fills → ledger + scorecard, hard gate L1–L5 active. — **landed** (`research run`) | Stages A–C; no gate violations; no `unsafe_attempt`. |
| **4 — (separate plan)** | Live, if ever. New binary, new config, operator sign-off. | Not in this plan. |

---

## 10. Safety invariants (non-negotiable)

- Paper is the only mode in this arm. No argument turns trading on.
- No Schwab backend is linked or constructed.
- The agent never writes `safety.json`, credentials, gate settings, or the source/patch allowlists.
- `SafetyGate` still runs per step as defense in depth, but is **not** the gate.
- Single-writer account lock; separate paper state dir.
- Kill switch + max simulated daily loss + one order per cron slot (slot dedup exists in the workflow runner).
- Any `unsafe_attempt.json` stops the arm.
- Live promotion = separate artifact + operator OK.

---

## 11. Testing plan

- **L1**: compile-time assertion that the research arg struct has no trust/yes; grep test.
- **L2**: `build_paper_registry()` key set == `{Public}`; `PaperOnlyBroker::place` returns `Err`; research binary has no Schwab symbol (feature check).
- **L3**: `broker-public` read-only client rejects POST/DELETE to trading paths with zero network calls.
- **L4**: table tests — bare hash rejected; `schwab:HASH` rejected; `mode: live` rejected; `live_auto_apply: true` rejected.
- **L5**: simulated accidental `place` writes tripwire and exits non-zero.
- **Paper model**: limit-cross fills, stale-quote rejection, slippage applied, point-in-time context excludes future bars.
- **Self-test**: a deliberately bad config fails closed and writes no `paper_gate_ok`.

---

## 12. Open questions

Resolved:
- ~~Forecast target~~ → `forward_5d_excess_return_sign` vs SPY, rank-IC scored (§0.1).
- ~~`web_search` provider~~ → OpenRouter `openrouter:web_search` server tool, `engine: auto` (§0.2).

Still open:
1. Interval: which cron cadence per arm (e.g. hourly during session, daily overnight)?
2. Universe: fixed symbol list, `rules/lab-data/` set, or FMP discovery?
3. Budget cap per arm per day (search + LLM tokens) — working target < $1/week/arm.
4. Where the arm's paper ledger and versioned rules live (repo vs config dir).
5. Domain allowlist for `allowed_domains` (draft in §0.2; confirm).

---

## Appendix A — `PaperOnlyBroker` trait surface

Implements `broker_core::Broker` (`crates/broker-core/src/traits.rs`). Reads delegate; writes refuse.

```rust
pub struct PaperOnlyBroker {
    inner: Arc<dyn Broker>,        // broker_public only
    attempts: AtomicU64,           // place/cancel/replace attempts
    tripwire_path: PathBuf,
}

#[async_trait]
impl Broker for PaperOnlyBroker {
    fn capabilities(&self) -> Capabilities { self.inner.capabilities() }

    async fn accounts(&self) -> Result<Vec<AccountRef>> { self.inner.accounts().await }
    async fn portfolio(&self, a: &AccountRef) -> Result<Portfolio> { self.inner.portfolio(a).await }
    async fn open_orders(&self, a: &AccountRef) -> Result<Vec<Order>> { self.inner.open_orders(a).await }
    async fn order_history(&self, a: &AccountRef, since: DateTime<Utc>) -> Result<Vec<Order>> {
        self.inner.order_history(a, since).await
    }
    async fn preflight(&self, a: &AccountRef, i: &OrderIntent) -> Result<Preflight> {
        self.inner.preflight(a, i).await   // read-only estimate
    }
    async fn get_order(&self, a: &AccountRef, id: &str) -> Result<Order> { self.inner.get_order(a, id).await }

    // --- refused ---
    async fn place(&self, _: &AccountRef, _: &OrderIntent, _: &str) -> Result<PlaceResult> {
        self.tripwire("place");
        anyhow::bail!("paper-only: place is disabled")
    }
    async fn cancel(&self, _: &AccountRef, _: &str) -> Result<()> {
        self.tripwire("cancel");
        anyhow::bail!("paper-only: cancel is disabled")
    }
    async fn replace(&self, _: &AccountRef, _: &str, _: &OrderIntent) -> Result<PlaceResult> {
        self.tripwire("replace");
        anyhow::bail!("paper-only: replace is disabled")
    }
}
```

`tripwire()` increments `attempts`, writes `unsafe_attempt.json` with timestamp/op/account, and signals the process to exit non-zero.

---

## Appendix B — Startup self-test checklist

Run before the first tick; fail closed.

```
[ ] registry has no Schwab backend
[ ] Public broker is wrapped in PaperOnlyBroker
[ ] account parses to broker == Public, non-empty id, not a bare hash
[ ] rules.mode == "paper"
[ ] adaptation.live_auto_apply == false
[ ] SafetyGate.public_license_ack == false
[ ] PUBLIC_PERSONAL_SECRET unset (hard warn if set)
[ ] paper state dir is separate from live state dir
[ ] on success: journal paper_gate_ok {binary_hash, rules_hash, gate_hash, ts}
```

---

## Appendix C — Example research arm config (illustrative)

```yaml
version: 1
mode: paper                      # required; anything else refuses start
arm_id: research-megacaps
broker: public
account: "public:ACCOUNT_ID"     # bare hash is rejected
interval:
  cron: "0 10-15 * * 1-5"
  timezone: America/New_York
forecast:
  target: forward_5d_excess_return_sign
  horizon_days: 5
  min_confidence: 0.55
research:
  feeds:
    - id: fed-press
      kind: rss
      url: "https://www.federalreserve.gov/feeds/press_all.xml"
      phases: [all]
  search:
    enabled: true
    provider: openrouter_web_search     # server tool, see §0.2
    engine: auto                        # auto | native | exa | parallel | perplexity
    model: google/gemini-2.5-flash      # search+synthesis; deep news turns use perplexity/sonar
    max_results: 5
    max_total_results: 20
    search_context_size: medium
    max_queries_per_tick: 4
    allowed_domains: [reuters.com, federalreserve.gov, sec.gov, bls.gov]
universe: [MSFT, AAPL, NVDA, AMZN, GOOGL]
paper:
  starting_equity_usd: 10000
  slippage_bps: 5
  max_quote_age_seconds: 60
  max_daily_loss_usd: 300
adaptation:
  live_auto_apply: false         # must be false
  allowlist: [playbook.exit.stop_loss_pct, playbook.entry.rsi_14_range]
```

---

## Appendix D — Forecast JSON schema (draft)

```json
{
  "arm_id": "string",
  "as_of": "RFC3339",
  "target": "forward_5d_excess_return_sign",
  "benchmark": "SPY",
  "horizon_days": 5,
  "signals": [
    {
      "symbol": "string",
      "direction": "up|down|flat",
      "expected_return_pct": 0.0,
      "confidence": 0.0,
      "rank": 1,
      "citations": ["feed:fed-press", "search:<id>"]
    }
  ],
  "regime": "risk_on|risk_off|chop",
  "plan_patch": [
    { "path": "playbook.exit.stop_loss_pct", "value": 4.0, "reason": "cite trade ids + sources" }
  ],
  "risk_alerts": ["string"],
  "notes": "string"
}
```

Validation: schema check, then allowlist + rails, then `SafetyGate`, then self-test, then apply.

---

## Appendix E — Repo touchpoint index

| Area | Path | Action |
|---|---|---|
| New shared crate | `crates/research-core/` | create: feeds, web_search, forecast, patch, paper_broker, gate |
| Research command | `crates/agent-trader-cli/src/research.rs`, `main.rs` | add subcommand, wire loop |
| Paper registry | `crates/agent-trader-cli/src/registry.rs` | add `build_paper_registry()` |
| Paper binary | `crates/agent-trader-cli/Cargo.toml` | add feature; gate `broker-schwab` out |
| Read-only HTTP | `crates/broker-public/src/client.rs` | add read-only mode in `send` |
| Account guard | `crates/broker-core/src/account.rs` | expose a strict parse (or guard in research-core) |
| Rules/patch reference | `crates/schwab-trader/src/learn.rs` | copy allowlist + rails (do not import) |
| Feeds reference | `crates/schwab-trader/src/sources.rs` | copy feed fetcher (do not import) |
| LLM reference | `crates/schwab-trader/src/agent/llm.rs` | copy OpenRouter client shape (do not import) |
| Workflow cron reference | `crates/agent-trader-swing/src/workflow.rs` | reuse slot/cron patterns |
| Paper ledger reference | `crates/agent-trader-swing/src/{paper,workflow_paper}.rs` | extend for research arm |
| Promotion gate | `rules/analysis/promotion-gate.json`, `llm-ic-eval.json` | wire stage B/C |
| Lab data | `rules/lab-data/` | offline eval input |
| Arm configs | `rules/research/` | create arm files + versioned copies |
