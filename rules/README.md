# Rules files

**Public templates (committed):**

- `options-rules.example.yaml` — options agent template (copy and edit locally)
- `trader-rules.example.yaml` — equity swing trader template
- `universe/` — shared symbol pools

**Personal configs (never commit):**

Copy a template to a local name (e.g. `rules/my-options.yaml`) and add your Schwab `hashValue` from `schwab accounts numbers --json`. Files matching `*-8709.yaml`, `*-9947.yaml`, and other account-specific names are gitignored.

Runtime state (`agent-state-*.json`, `trader-state-*.json`, journals, logs) is also gitignored.
