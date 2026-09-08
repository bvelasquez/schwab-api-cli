# Rules files

**Public templates (committed):**

- `options-rules.example.yaml` — options agent template (copy and edit locally)
- `trader-rules.example.yaml` — equity swing trader template
- `universe/` — shared symbol pools

**Personal configs (never commit):**

Copy a template to a local name (e.g. `rules/my-options.yaml`) and add your Schwab `hashValue` from `schwab accounts numbers --json`. Files matching `*-8709.yaml`, `*-9947.yaml`, and other account-specific names are gitignored.

Runtime state (`agent-state-*.json`, `trader-state-*.json`, journals, logs) is also gitignored.

## Hot-reload (rules YAML)

Standing `schwab watch` / `schwab agent run` and `schwab-trader watch` loops **reload rules YAML from disk** without restarting the process.

- The loop fingerprints the rules file (and, for the equity trader, `watchlists.candidate_pool_file` if set).
- When content changes (or you send **SIGHUP** / `schwab agent reload <file>` / `schwab-trader agent reload --rules-file <file>`), the next tick validates and swaps the new rules.
- **Invalid YAML is fail-closed:** the previous rules stay loaded, the error is logged, and the tick loop keeps running (open positions, journals, pid unchanged).
- Changing `agent_id` / `trader_id` is rejected (restart required) so state files stay bound to the process identity.
- **`--simulate` is unchanged.** Binary/code changes still need a rebuild and process restart.

Poll interval while sleeping between ticks is ≤1s, so a saved YAML usually applies before the next scheduled tick.
