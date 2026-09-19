# Proposal: freeze the LLM on the options paper agent (awaiting approval)

**Status: NOT APPLIED.** Barry has not answered. `rules/options-pilot-8709.yaml` is a jarvis
unit rules file — it may not be edited without an explicit OK (hard rule 3).

**File:** `rules/options-pilot-8709.yaml` on jarvis (authoritative; the Mac copy is stale and the
file is gitignored, so apply there). sha256 (16) at time of writing: `376a7b3e3db0f17a`.

**Evidence it is not frozen** (from `scripts/agent-health.py` + the file):

| path | swing-9947 (frozen) | options-8709 |
|---|---|---|
| `llm.veto_entries` | `false` | **`true`** (line 279) |
| `llm.allow_llm_exits` | `false` | `false` |
| `llm.allow_rule_adaptation` | `false` | *(key absent)* |
| `llm.allow_rule_suggestions` | — | **`true`** (line 282) |
| `adaptation.llm_profile_select` | `false` (line 446) | **block absent → defaults `true`** |

`llm_profile_select` defaults to `true` in the struct (`crates/schwab-trader/src/rules.rs:1291`),
so the LLM can still switch the options bot's active profile and veto its entries.

## Minimal diff (option B — matches swing, keeps rule suggestions)

```diff
--- rules/options-pilot-8709.yaml
+++ rules/options-pilot-8709.yaml
@@ llm: (line 279)
-  veto_entries: true
+  veto_entries: false
@@ end of file (block does not exist yet)
+adaptation:
+  llm_profile_select: false
```

Option C additionally flips `allow_rule_suggestions: true` → `false` (line 282).

## Apply procedure (after approval)

```bash
cd ~/projects/schwabinvestbot
cp rules/options-pilot-8709.yaml rules/options-pilot-8709.yaml.bak-<date>
# 1. edit the two keys
python3 -c "import yaml,sys; yaml.safe_load(open('rules/options-pilot-8709.yaml'))"   # 2. parse check
schwab-trader agent reload rules/options-pilot-8709.yaml                              # 3. SIGHUP, keeps state
# 4. verify: no --trust/--yes anywhere in the unit; agent-health.py still STALE: none
```

Then confirm on the next session that the entry path stops showing LLM vetoes in the journal.

## Related, no action needed

`rules/trader-swing-9947.yaml` (sha256 `81236a093a3c…`) is fully frozen. Its state still reports
`active_profile_source: 'llm'` — a stale persisted value from before the reload; expect `regime`
after the next selection. Re-check Monday, along with `llm_veto_or_missing_review` → 0.
