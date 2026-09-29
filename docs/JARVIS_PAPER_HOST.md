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

### Headless auto re-login (Schwab OAuth)

Schwab refresh tokens die **7 days after interactive login**, with no way to extend them.
Until now, re-auth meant Barry running [`scripts/jarvis-auth-login.sh`](../scripts/jarvis-auth-login.sh)
from his Mac by hand every ~7 days. `schwab-auto-login.timer` does the same authorize →
login → 2FA → consent → code-exchange dance **on Jarvis**, driving a real Chromium against
the real Schwab login pages via Playwright — with one exception: **2FA still needs a human**
(see below), so this isn't fully unattended, it just moves "Barry drives a browser through
OAuth" down to "Barry replies to one Telegram message."

Jarvis has no physical display or desktop session, but this is **not** run with
Playwright's `headless=True`: Schwab's Akamai WAF flatly 403s ("Access Denied") a true
headless-mode Chromium's fingerprint before the login page ever loads — confirmed live
during development, `curl` with the same client hit the same 403 too. Normal-mode Chrome
under a virtual framebuffer (`xvfb-run`, already on Jarvis via the `xvfb` package) reaches
the real login page fine, so `schwab-auto-login.sh` wraps the whole run in `xvfb-run -a`.
Nothing about this needs a real display — Xvfb only satisfies Chrome's requirement to
attach to *some* display.

| Piece | What it does |
|-------|----------------|
| [`scripts/schwab-auto-login.py`](../scripts/schwab-auto-login.py) | The flow itself: decides whether a login is even needed, drives headless-under-Xvfb Chromium through login/2FA/consent, exchanges the code under the same `flock` the token keeper uses, and re-runs the keeper so the mirror is republished immediately |
| [`scripts/schwab-auto-login.sh`](../scripts/schwab-auto-login.sh) | Wrapper: sources `schwab-paper.conf` (app key + Telegram creds only), runs the venv's python, passes args through. `--setup` creates the venv and installs Playwright + Chromium |
| `schwab-auto-login.service` / `.timer` | Runs the wrapper daily at 08:00 America/Los_Angeles, plus Sun 17:00 (`deploy/systemd/user/`). Keeper-triggered runs outside 07:00–22:00 Pacific defer silently (`SCHWAB_AUTOLOGIN_AWAKE_START`/`_END`/`_TZ`) |

**No second-factor secret is stored on Jarvis.** There is no TOTP seed, no VIP Access
credential, nothing that by itself could complete a login. At Schwab's 2FA step the script
texts Barry over the existing schwab Telegram bot and long-polls for his reply — see
"2FA: a Telegram round trip with Barry" below. **Threat model:** a compromised Jarvis
leaks `SCHWAB_LOGIN_ID` + `SCHWAB_PASSWORD` at worst — those alone are not enough to
authenticate; the second factor lives on Barry's phone and never touches disk on Jarvis.

**It runs a real login only when needed.** Every run checks `schwab auth status --json`'s
`data.refresh_expiry_known` / `data.refresh_expires_in_seconds` fields (populated from the
token file's `login_at`, which is carried forward across refreshes since refreshing an
access token does **not** reset the refresh token's 7-day clock):

- Known remaining life **< 36h** → run.
- Unknown (`refresh_expiry_known` false/missing, e.g. an older token file, or the CLI call
  itself failed) → run **at most once per 6 days**, tracked in a local last-success stamp
  (`~/.local/state/schwab-paper/auto-login-last-success.json`) — never falls back to
  `obtained_at`, which would silently hide the need to re-auth.
- Otherwise → no-op. No browser, no Telegram prompt, no risk.

`--force` skips this decision (but not the lockout cap below). `jarvis-token-keeper.sh`
also starts `schwab-auto-login.service` (best-effort, `--no-block`) the moment it hits
"unauthenticated" or "refresh failed", so a dead token gets a same-cycle self-heal attempt
instead of waiting for the next timer tick — same lockout budget applies.

**Lockout protection is the load-bearing safety mechanism here** — repeated failed logins
can lock Barry's Schwab account, which is worse than a stale paper agent. The script caps
itself to **2 password submissions per rolling 24h** and enforces a **6h backoff after any
failure**, tracked in `~/.local/state/schwab-paper/auto-login.json`. A failure *before* the
password is ever submitted (e.g. the login page's fields didn't match a known selector)
does not count against this budget — only an actual submitted-password attempt does.
`--force` skips the "is a login needed" decision but **not** this cap; add `--i-know` too
to bypass the cap itself (use this only when intentionally testing). It never retries a
rejected password within a single run; a rejected 2FA code gets exactly one retry prompt.

**Credentials are split across two files, deliberately:**

```bash
# ~/.config/environment.d/schwab-paper.conf (chmod 600) — app key + Telegram bot creds.
# This file IS a systemd EnvironmentFile for the paper agents, so anything in it is
# readable via /proc/<pid>/environ for the lifetime of those processes.
SCHWAB_APP_KEY=...
TELEGRAM_BOT_TOKEN=...
TELEGRAM_CHAT_ID=...

# ~/.config/schwabinvestbot/autologin.env (chmod 600 or 400, owned by jarvis) —
# login id/password ONLY. schwab-auto-login.py reads this file directly, in-process,
# via a plain KEY=VALUE parser. It is NEVER exported into os.environ, NEVER passed as a
# subprocess env=, and NEVER referenced by an EnvironmentFile= in any systemd unit — the
# password must never be able to show up in a process's /proc/<pid>/environ.
SCHWAB_LOGIN_ID=...
SCHWAB_PASSWORD=...
SCHWAB_2FA_METHOD=sms   # or "push" (approve in the Schwab app); optional, defaults to "sms"
```

The script refuses to run at all if `autologin.env` is missing, has a mode other than 600
or 400, or isn't owned by the current user — no silent fallback.

**2FA: a Telegram round trip with Barry.** At the 2FA step the script:

1. Calls `getUpdates` once up front to find the current max `update_id`, and starts polling
   from `offset = max + 1` — anything already sitting in the queue (e.g. a stale code from a
   previous run) is ignored.
2. Waits for Schwab's "Confirm Your Identity" picker (`#/authenticators`; cards "Schwab
   App", "Text me at …", "Call me at …", "Call Schwab") and clicks the card for
   `SCHWAB_2FA_METHOD`: `sms` (default) → "Text me at"; `push` → "Schwab App". With
   `push` the script just messages Barry to approve in the Schwab app and waits for the
   page to move on; steps 3–5 apply to `sms` only.
3. Sends: `🔐 Schwab re-login on jarvis: Schwab just texted you a code. Reply here with the
   6-digit code within 10 min. (nonce ABCD)` — a short random nonce, mostly so Barry can
   tell which prompt a reply is answering if more than one is ever in flight.
4. Long-polls `getUpdates` (`timeout=30`, looped) for up to `SCHWAB_2FA_WAIT_SECS` (default
   600s). It accepts a reply **only** if all of: `message.chat.id == TELEGRAM_CHAT_ID`,
   `message.from.id == TELEGRAM_CHAT_ID` (i.e. Barry himself, in that private chat — not a
   group, not someone else), and the text matches `^\s*\d{6}\s*$` or `<nonce> 123456`
   (case-insensitive). Every update's id advances the poll offset whether or not it
   matched, so nothing is ever redelivered.
5. Enters the code, submits, and replies on Telegram: `✅ code accepted, finishing login`
   or `❌ code rejected — one more try` (one retry within the same run, then gives up).
6. On timeout: `⏰ No 2FA code received; tokens expire around <time>; will retry at the
   next schedule.` and exits non-zero. This only counts toward the lockout budget if the
   password was actually submitted — the whole flow up through the 2FA field is "after
   submit" per the lockout accounting above, so a real timeout here does count.

Nothing in this repo polls this bot token, and the furoshiki listener uses a separate
token (`FUROSHIKI_BOT_TOKEN`). A transient HTTP 409 was nevertheless seen live on
2026-09-29 (source unconfirmed; the root-owned Hermes gateway couldn't be inspected), so
409s are retried every 3s and only abort after `SCHWAB_TELEGRAM_CONFLICT_GRACE_SECS`
(default 90s) of continuous conflict. A poller that *consumes* updates could still eat
Barry's reply; if that happens the run times out rather than guessing. **The password and the 2FA code
are never logged or echoed anywhere** — not to the log file, not to Telegram, not to
stdout.

> **Selectors not verified live:** the login-id/password field selectors were confirmed
> against the real Schwab login page (`--dry-run`, no credentials submitted). The
> **2FA method-choice and code-entry selectors were not** — reaching them requires
> submitting a real password, which this project does not do outside of Barry's own runs.
> They're written defensively (multiple independent selectors, generic text-based button
> matching) and any miss saves a redacted screenshot + HTML dump instead of failing silent.
> Check `~/.local/state/schwab-paper/auto-login/<timestamp>/` after Barry's first real run
> and feed back any selector fixes needed.

**On failure** the script never restarts anything or touches live tokens: it screenshots
the page and dumps its HTML (input values stripped) to
`~/.local/state/schwab-paper/auto-login/<timestamp>/` (`chmod 700`/`600`, pruned after 14
days), sends a Telegram alert naming the failed step, and exits non-zero — Barry re-auths
by hand with `jarvis-auth-login.sh` same as always.

First-time setup, on Jarvis:

```bash
mkdir -p ~/.config/schwabinvestbot
cat > ~/.config/schwabinvestbot/autologin.env <<'EOF'
SCHWAB_LOGIN_ID=...
SCHWAB_PASSWORD=...
EOF
chmod 600 ~/.config/schwabinvestbot/autologin.env

./scripts/schwab-auto-login.sh --setup              # venv + Playwright/Chromium
./scripts/schwab-auto-login.sh --dry-run            # confirm it can load the login page + find fields
./scripts/schwab-auto-login.sh --self-test          # offline checks (Telegram filter, perms, lockout), no network
./scripts/schwab-auto-login.sh --force --i-know     # first real run — reply to the Telegram prompt when it arrives
tail -f ~/.local/state/schwab-paper/auto-login.log
```

Deployed by `jarvis-deploy.sh` alongside the scorecard/chain-snapshot timers
(`schwab-auto-login.timer` enabled the same way).

## Scripts (run from Mac)

| Script | What it does |
|-------|----------------|
| [`scripts/jarvis-deploy.sh`](../scripts/jarvis-deploy.sh) | SSH → `git fetch` + `pull --ff-only origin main` → `cargo install` both crates `--force` → copy units → `daemon-reload` → enable + restart both services → print versions, `systemctl --user status`, `pgrep` smoke |
| [`scripts/jarvis-rules-reload.sh`](../scripts/jarvis-rules-reload.sh) | SSH → `schwab agent reload rules/options-pilot-8709.yaml` and `schwab-trader agent reload rules/trader-swing-9947.yaml` |
| [`scripts/jarvis-auth-login.sh`](../scripts/jarvis-auth-login.sh) | Mac browser OAuth → write `tokens.json` on Jarvis (`schwab auth login --code`). The keeper republishes the agents' mirror within 5 min. |
| [`scripts/jarvis-token-sync.sh`](../scripts/jarvis-token-sync.sh) | Mac-side read-only mirror: asks Jarvis to refresh if the access token is nearly stale (Jarvis is the owner), then writes the Mac's `tokens.json` with `refresh_token` blanked. Re-run when the Mac token goes stale. |
| [`scripts/jarvis-token-keeper.sh`](../scripts/jarvis-token-keeper.sh) | Runs **on jarvis** via `schwab-token-keeper.timer`: the single OAuth refresher, republishes the access-token mirror |
| [`scripts/jarvis-bot-watchdog.sh`](../scripts/jarvis-bot-watchdog.sh) | Runs **on jarvis** via `schwab-bot-watchdog.timer`: restarts an agent whose `last_tick` is stale for the current session; escalates on repeat |
| [`scripts/schwab-auto-login.sh`](../scripts/schwab-auto-login.sh) | Runs **on jarvis** via `schwab-auto-login.timer` (see [Headless auto re-login](#headless-auto-re-login-schwab-oauth)): headless Playwright OAuth re-login, only when actually needed |

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
