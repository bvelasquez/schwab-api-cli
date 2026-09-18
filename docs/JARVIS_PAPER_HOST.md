# Jarvis paper-host migration

Move **paper** (`--simulate`) options and swing agents from Barry’s Mac onto **Jarvis** (Linux, SSH host `jarvis`, user `jarvis`). Live trading stays on the Mac until a later, explicit cutover.

## Venue rule (Barry, 2026-09-17)

**The live/paper test runs ONLY on Jarvis.** Config checks, backtests, deploys, log/journal reads and results all
happen there; this MacBook is source code only. Two consequences nothing enforces for you:

- `rules/*-9947.yaml` are gitignored, so `git pull` never carries them. Jarvis's copy is authoritative and the Mac's
  copies go stale silently (on 2026-09-17 the Mac's had PT 8.5 / trail 4.0 / RSI 50 while Jarvis had 7.5 / 3.0 / 48 —
  deploying it would have reverted three approved changes). `scp` Jarvis's file down and diff before editing; never
  push the Mac's up.
- A non-interactive SSH shell does not inherit the unit's environment, so remote CLI calls need
  `set -a; . $HOME/.config/environment.d/schwab-paper.conf; set +a; export SCHWAB_REPO=$HOME/projects/schwabinvestbot`
  prefixed or they die with `SCHWAB_APP_KEY ... is required`.
- **Jarvis owns OAuth.** One Schwab OAuth session per account, so no Mac-side login and no Mac-side refresh: the Mac
  consumes a mirrored access token (`scripts/jarvis-token-sync.sh`, refresh token blanked). Any second refrester —
  another host, or a second long-running Mac process — silently revokes Jarvis's refresh token and both paper agents
  go `auth_fatal` (happened 2026-09-18).

This document is the operator plan. Phase 0 is **code and scripts only** — do not stop Mac agents or SSH to Jarvis from CI/cloud agents.

## Hosts and binaries

| Role | Where | How it runs |
|------|--------|-------------|
| Operator / live (until later phases) | Mac | `schwab watch` / `schwab-trader watch` as today |
| Paper host (target) | Jarvis | systemd **user** units, foreground `agent run … --simulate` |
| Repo clone on Jarvis | `$HOME/projects/schwabinvestbot` (`%h/projects/schwabinvestbot`) | `git pull --ff-only origin main` via deploy script |
| Binaries on Jarvis | `$HOME/.cargo/bin/schwab`, `schwab-trader` | `cargo install --path … --force` |

SSH: `Host jarvis` with `User jarvis` in `~/.ssh/config`. Override with `JARVIS_SSH` / `JARVIS_REPO`.

## Safety

- Paper units and deploy scripts **must not** pass `--trust` or `--yes` **as trade flags**.
  The only `--yes` in the tree is on `schwab auth refresh` inside the token keeper — that is
  the non-interactive OAuth path, it cannot place an order.
- `--simulate` remains the paper-host default. Live defaults are unchanged.
- systemd is the supervisor: units use `Type=simple` (foreground). Do **not** add `--background` to unit `ExecStart`.
- `schwab-trader agent run --background` is for ad-hoc detach (pid/log next to rules), same shape as `schwab agent run --background`.

## systemd user units

Templates in-repo: [`deploy/systemd/user/`](../deploy/systemd/user/).

| Unit | ExecStart |
|------|-----------|
| `schwab-options-8709.service` | `%h/.cargo/bin/schwab agent run rules/options-pilot-8709.yaml --simulate` |
| `schwab-swing-9947.service` | `%h/.cargo/bin/schwab-trader agent run rules/trader-swing-9947.yaml --simulate` |

Shared settings: `Type=simple`, `WorkingDirectory=%h/projects/schwabinvestbot`, `Restart=on-failure`, `RestartSec=10`, `After=`/`Wants=network-online.target`.

Override the working directory with a drop-in if the clone is elsewhere:

```bash
systemctl --user edit schwab-options-8709.service
# [Service]
# WorkingDirectory=/actual/clone
```

### Linger

User units stop at logout unless lingering is enabled:

```bash
sudo loginctl enable-linger jarvis
loginctl show-user jarvis | grep Linger
```

### Token ownership — exactly one refresher

Schwab keeps **one active OAuth session per account**: two processes that refresh
invalidate each other's refresh token, and the losers spin on `invalid_grant` forever.
Both paper agents died this way on 2026-09-18 (swing 4 h 15 m, options ≈ 20 h) while
systemd reported them `active` — `Restart=on-failure` never fires, because the process
stays alive and simply stops trading. So exactly one process refreshes:

| Unit | Job |
|------|-----|
| `schwab-token-keeper.service` | refreshes the owner bundle only when `expires_in < 900s` (under `flock`), then rewrites the agents' mirror |
| `schwab-token-keeper.timer` | drives it every 5 min — the **only** OAuth refresher on the host |

The agents read an access-token-only **mirror** through a drop-in
([`schwab-agents-token-mirror.conf`](../deploy/systemd/user/schwab-agents-token-mirror.conf),
`SCHWAB_TOKEN_DIR=%h/.config/schwabinvestbot/agents`), so an agent physically cannot start
its own refresh. A missing `refresh_token` in the mirror is the design, not corruption —
that file holds the access token only. Deploy order matters: `jarvis-deploy.sh` seeds the
mirror *before* restarting the agents.

### Watchdog — stale ticks

`schwab-bot-watchdog.timer` (5 min) reads each agent's `last_tick` from its state JSON. If
it is stale for the current session it re-runs the keeper and restarts the unit, with a
Telegram alert; if it is stale again on the next cycle it escalates **without** restarting
(a dead refresh token needs `scripts/jarvis-auth-login.sh`, not a restart loop).

Thresholds are session-aware and mirror `crates/*/src/agent/schedule.rs`:

| agent | regular | premarket | idle / closed |
|-------|---------|-----------|----------------|
| swing (`schwab-trader`) | 900 s (sleeps 90 s) | 900 s (sleeps 300 s) | 3600 s (**sleeps 1800 s by design**) |
| options (`schwab-cli`) | 900 s (sleeps 120 s) | — | 900 s (sleeps 120 s) |

A flat threshold is a bug, not a simplification: after hours the swing agent is *supposed*
to be quiet for 30 minutes, and a 900 s constant restarted a perfectly healthy agent three
times in 35 minutes. `systemctl is-active` is not health — check `last_tick`.

## Scripts (run from Mac)

| Script | What it does |
|-------|----------------|
| [`scripts/jarvis-deploy.sh`](../scripts/jarvis-deploy.sh) | SSH → `git fetch` + `pull --ff-only origin main` → `cargo install` both crates `--force` → copy units → `daemon-reload` → enable + restart both services → print versions, `systemctl --user status`, `pgrep` smoke |
| [`scripts/jarvis-rules-reload.sh`](../scripts/jarvis-rules-reload.sh) | SSH → `schwab agent reload rules/options-pilot-8709.yaml` and `schwab-trader agent reload rules/trader-swing-9947.yaml` |
| [`scripts/jarvis-auth-login.sh`](../scripts/jarvis-auth-login.sh) | Mac browser OAuth → write `tokens.json` on Jarvis (`schwab auth login --code`). The keeper republishes the agents' mirror within 5 min. |
| [`scripts/jarvis-token-sync.sh`](../scripts/jarvis-token-sync.sh) | Mac-side read-only mirror: asks Jarvis to refresh if the access token is nearly stale (Jarvis is the owner), then writes the Mac's `tokens.json` with `refresh_token` blanked. Re-run when the Mac token goes stale. |
| [`scripts/jarvis-token-keeper.sh`](../scripts/jarvis-token-keeper.sh) | Runs **on jarvis** via `schwab-token-keeper.timer`: the single OAuth refresher, republishes the access-token mirror |
| [`scripts/jarvis-bot-watchdog.sh`](../scripts/jarvis-bot-watchdog.sh) | Runs **on jarvis** via `schwab-bot-watchdog.timer`: restarts an agent whose `last_tick` is stale for the current session; escalates on repeat |

All fail fast (`set -euo pipefail`), refuse `--trust`/`--yes`, and never add those flags to remote commands.

```bash
# from a Mac with SSH to jarvis
./scripts/jarvis-deploy.sh
./scripts/jarvis-rules-reload.sh
./scripts/jarvis-auth-login.sh          # browser on Mac, tokens on Jarvis
./scripts/jarvis-auth-login.sh --paste   # paste the redirect URL instead
./scripts/jarvis-token-sync.sh           # mirror Jarvis's access token onto this Mac (read-only consumer)
```

Re-auth notes:

- Jarvis has no GUI. The script listens on **this Mac** at `https://127.0.0.1:8182`, then exchanges the code on Jarvis using `~/.config/environment.d/schwab-paper.conf`.
- **Jarvis is the single OAuth owner** (Schwab permits one active OAuth session per account: a second refrester revokes the other holder's refresh token). Only Jarvis logs in and only Jarvis refreshes.
- Do **not** copy Mac `tokens.json` onto Jarvis, and do not log in on the Mac: Mac CLI work reads a mirrored **access** token from `scripts/jarvis-token-sync.sh`, which writes the Mac's token file with `refresh_token` blanked so a Mac-side refresh fails closed.
- Running paper units do not need a restart; they reload tokens from disk each tick.
- `schwab auth status --json` reports refresh life from the local file's `obtained_at`, so a revoked token still looks valid for days. Confirm with `schwab auth refresh --yes --json` — `invalid_grant` means it is dead.

## Ad-hoc background (not systemd)

Same pid/log + SIGHUP contract as options:

```bash
# next to rules: trader-trader-swing-9947.pid / trader-trader-swing-9947.log
schwab-trader agent run rules/trader-swing-9947.yaml --background --simulate --json
schwab-trader agent reload rules/trader-swing-9947.yaml
schwab-trader agent stop rules/trader-swing-9947.yaml
```

Options:

```bash
schwab agent run rules/options-pilot-8709.yaml --background --simulate --json
schwab agent reload rules/options-pilot-8709.yaml
schwab agent stop rules/options-pilot-8709.yaml
```

## Phase checklist

### Phase 0 — in-repo (this PR)

- [x] `schwab-trader agent run --background` detaches (setsid), writes pid/log next to rules, prints pid
- [x] `schwab-trader agent stop` / `agent reload` (SIGHUP) match options behavior
- [x] `--simulate` is forwarded; `--trust`/`--yes` are not implied
- [x] systemd user unit templates under `deploy/systemd/user/`
- [x] `scripts/jarvis-deploy.sh`, `scripts/jarvis-rules-reload.sh`, and `scripts/jarvis-auth-login.sh` (executable, fail fast, no live flags)
- [x] This document + README link
- [x] Tests for detach helpers and trader background CLI wiring
- [ ] **Not in Phase 0:** SSH to Jarvis, stop Mac agents, enable linger, or start paper units in production

### Phase 1 — paper soak on Jarvis (operator)

- [ ] Linger enabled for `jarvis`
- [ ] Clone/env/token/`safety.json`/personal rules present on Jarvis (gitignored `*-8709.yaml` / `*-9947.yaml`)
- [ ] Run `./scripts/jarvis-deploy.sh` from Mac
- [ ] Confirm both units `active (running)` and `--simulate` in `pgrep -af`
- [ ] Reload YAML with `./scripts/jarvis-rules-reload.sh` without restart
- [ ] Re-auth from Mac with `./scripts/jarvis-auth-login.sh` when the refresh token is close to expiry
- [ ] Mac paper watches stay up until soak looks healthy

### Phase 2 — stop Mac paper (operator)

- [ ] Stop Mac `--simulate` watches only after Jarvis paper is healthy
- [ ] Leave live Mac agents untouched

### Phase 3 — live cutover (not started)

- [ ] Separate live units (explicit `--trust --yes`) — **do not reuse paper ExecStart**
- [ ] Operator sign-off; out of scope for Phase 0

## Related docs

- [OPTIONS_RULES.md](OPTIONS_RULES.md) — options agent
- [TRADER_RULES.md](TRADER_RULES.md) — equity trader
- [AGENT_SCHEDULE.md](AGENT_SCHEDULE.md) — session modes
- [TRADER_ROLLOUT.md](TRADER_ROLLOUT.md) — live capital checklist (Mac / later phases)
