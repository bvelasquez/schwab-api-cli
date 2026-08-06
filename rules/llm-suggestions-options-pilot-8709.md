# Options agent LLM suggestions (human apply only)

Engine does **not** auto-apply these. Review and edit rules YAML yourself.

Allowlisted paths:
- `exit_rules.thesis.min_short_otm_pct`
- `exit_rules.thesis.min_hold_minutes`
- `exit_rules.thesis.min_pop_pct_exit`
- `exit_rules.thesis.max_short_delta_exit`
- `exit_rules.thesis.regime_mismatch.min_profit_pct`
- `entry_rules.vertical.short_delta_min`
- `entry_rules.vertical.short_delta_max`
- `entry_rules.vertical.min_iv_rv_ratio`
- `entry_rules.vertical.min_short_otm_pct`

## 2026-08-05T14:49:59.247839+00:00

### Lessons

- Closed GLD FB796575B24D9362CD379B656C39362CE53CAD49C5218098E9D890C00A71AFD8|GLD|2026-09-04|iron_condor|C403S_C408L_P335L_P340S exit=stop_loss pnl=$-134.64 (-226.8% of credit) hold since 2026-07-31T14:12:48.462929+00:00
- The trade was opened despite the LLM recommending to skip due to the short put strike being inside the 1-sigma expected move, which is explicitly against the strategy's requirements. This suggests a failure in honoring the LLM's veto or the system setup allowed the trade to open regardless of the 'skip' recommendation. The trade resulted in a significant loss, validating the LLM's initial caution.

### Suggested patches

_None (or not allowlisted)._

### Scorecard glance

`vetoes 0 · ignored defer 2 · linked W/L 0/1`
