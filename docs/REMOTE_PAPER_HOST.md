# Remote paper host

Run the options and swing agents in paper mode (`--simulate`) under systemd user units on a Linux host. Live trading stays on a machine you are watching until you explicitly cut over.

## OAuth ownership

Schwab issues one refresh token per app login. Refresh it on **one** host only. A second long-running process that calls the token endpoint revokes the first refresh token and both sides fail with `invalid_grant`.

Log in with `schwab auth login` on the host that owns the token. Other machines that only need to read data should not refresh.

## Layout

| Role | How it runs |
|------|-------------|
| Paper host | systemd **user** units, foreground `agent run … --simulate` |
| Repo clone | `$HOME/projects/schwabinvestbot` (`git pull --ff-only origin main`) |
| Binaries | `$HOME/.cargo/bin/schwab`, `schwab-trader` via `cargo install --path … --force` |

SSH target defaults to `paper-host`. Set `Host paper-host` in `~/.ssh/config`, or override `PAPER_HOST_SSH` / `PAPER_HOST_REPO`.

Copy `rules/options-rules.example.yaml` and `rules/trader-rules.example.yaml` to local names (for example `rules/options-pilot.yaml` and `rules/trader-swing.yaml`) and put your Schwab `hashValue` in those copies. Do not commit them. Point the unit `ExecStart` at your filenames with a drop-in if they differ from the defaults.

## Safety

- Paper units and deploy scripts must not pass `--trust` or `--yes` as trade flags.
- `--simulate` is the paper-host default. Live defaults are unchanged.
- systemd is the supervisor: units use `Type=simple` (foreground). Do not add `--background` to unit `ExecStart`.
- `schwab-trader agent run --background` is for ad-hoc detach only.

## systemd user units

Templates: [`deploy/systemd/user/`](../deploy/systemd/user/).

| Unit | ExecStart |
|------|-----------|
| `schwab-options.service` | `%h/.cargo/bin/schwab agent run rules/options-pilot.yaml --simulate` |
| `schwab-swing.service` | `%h/.cargo/bin/schwab-trader agent run rules/trader-swing.yaml --simulate` |

Shared settings: `Type=simple`, `WorkingDirectory=%h/projects/schwabinvestbot`, `Restart=on-failure`, `RestartSec=10`, `After=`/`Wants=network-online.target`.

Linger is required so user units survive logout:

```bash
sudo loginctl enable-linger "$USER"
```

## Scripts

| Script | What it does |
|--------|----------------|
| [`scripts/paper-host-deploy.sh`](../scripts/paper-host-deploy.sh) | SSH → `git pull --ff-only origin main` → `cargo install` both crates → copy units → `daemon-reload` → enable and restart paper units |
| [`scripts/paper-host-rules-reload.sh`](../scripts/paper-host-rules-reload.sh) | SSH → `schwab agent reload` and `schwab-trader agent reload` for the rules paths in `PAPER_OPTIONS_RULES` / `PAPER_SWING_RULES` |
| [`scripts/paper-host-watchdog.sh`](../scripts/paper-host-watchdog.sh) | Restart a `--simulate` unit whose `last_tick` went stale, and send a Telegram alert if configured. No orders. |
| [`scripts/scorecard.py`](../scripts/scorecard.py) | Read-only daily paper scorecard |
| [`scripts/chain-snapshot.sh`](../scripts/chain-snapshot.sh) | Read-only option-chain snapshots. Does not refresh OAuth. |

```bash
# from a machine with SSH to the paper host
./scripts/paper-host-deploy.sh
./scripts/paper-host-rules-reload.sh
```

A non-interactive SSH shell does not inherit a user unit's environment. Remote CLI calls need the Schwab app key on `PATH` / in the environment, or they exit with `SCHWAB_APP_KEY ... is required`.

## Watchdog

`schwab-bot-watchdog.timer` runs every 5 minutes. It restarts a unit that stopped ticking and, after repeated failures, alerts instead of looping. A dead refresh token needs `schwab auth login` on the OAuth-owner host, not another restart.

Override the watched units with `SCHWAB_WATCH_UNITS` (see the script header) if your rules filenames differ.
