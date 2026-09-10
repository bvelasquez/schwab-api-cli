# Jarvis paper-host migration

Move **paper** (`--simulate`) options and swing agents from Barry’s Mac onto **Jarvis** (Linux, SSH host `jarvis`, user `jarvis`). Live trading stays on the Mac until a later, explicit cutover.

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

- Paper units and deploy scripts **must not** pass `--trust` or `--yes`.
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

## Scripts (run from Mac)

| Script | What it does |
|-------|----------------|
| [`scripts/jarvis-deploy.sh`](../scripts/jarvis-deploy.sh) | SSH → `git fetch` + `pull --ff-only origin main` → `cargo install` both crates `--force` → copy units → `daemon-reload` → enable + restart both services → print versions, `systemctl --user status`, `pgrep` smoke |
| [`scripts/jarvis-rules-reload.sh`](../scripts/jarvis-rules-reload.sh) | SSH → `schwab agent reload rules/options-pilot-8709.yaml` and `schwab-trader agent reload rules/trader-swing-9947.yaml` |

Both fail fast (`set -euo pipefail`), refuse `--trust`/`--yes`, and never add those flags to remote commands.

```bash
# from a Mac with SSH to jarvis
./scripts/jarvis-deploy.sh
./scripts/jarvis-rules-reload.sh
```

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
- [x] `scripts/jarvis-deploy.sh` and `scripts/jarvis-rules-reload.sh` (executable, fail fast, no live flags)
- [x] This document + README link
- [x] Tests for detach helpers and trader background CLI wiring
- [ ] **Not in Phase 0:** SSH to Jarvis, stop Mac agents, enable linger, or start paper units in production

### Phase 1 — paper soak on Jarvis (operator)

- [ ] Linger enabled for `jarvis`
- [ ] Clone/env/token/`safety.json`/personal rules present on Jarvis (gitignored `*-8709.yaml` / `*-9947.yaml`)
- [ ] Run `./scripts/jarvis-deploy.sh` from Mac
- [ ] Confirm both units `active (running)` and `--simulate` in `pgrep -af`
- [ ] Reload YAML with `./scripts/jarvis-rules-reload.sh` without restart
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
