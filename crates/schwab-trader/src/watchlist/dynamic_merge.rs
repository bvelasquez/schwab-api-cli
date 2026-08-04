//! Merge playbook-qualified or FMP symbols into `TraderState.dynamic_watchlist`.

use std::collections::BTreeSet;

use crate::agent::state::TraderState;
use crate::rules::TraderRules;

/// Max symbols in `dynamic_watchlist` (screened qualifiers can exceed legacy `max_dynamic_symbols`).
pub fn dynamic_watchlist_capacity(rules: &TraderRules) -> usize {
    let base = rules.watchlists.max_dynamic_symbols.max(1) as usize;
    if rules.watchlists.screened.enabled {
        base.max(rules.watchlists.screened.top_n.max(1) as usize)
    } else {
        base
    }
}

/// Drop prior symbols from this source, then add up to `max_add` new names (deduped).
pub fn merge_dynamic_symbols(
    state: &mut TraderState,
    rules: &TraderRules,
    previous_source_symbols: &[String],
    candidates: &[String],
    max_add: usize,
) -> Vec<String> {
    let prev: BTreeSet<String> = previous_source_symbols
        .iter()
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    let open: BTreeSet<String> = state
        .open_positions
        .values()
        .map(|p| p.symbol.to_uppercase())
        .collect();

    state.dynamic_watchlist.retain(|s| {
        let u = s.to_uppercase();
        !prev.contains(&u)
            && !rules.is_blocked_symbol(&u)
            && !rules.is_core_holding(&u)
            && !open.contains(&u)
    });

    let cap = dynamic_watchlist_capacity(rules);
    let max_add = max_add.max(1);

    let mut added = Vec::new();
    for sym in candidates {
        if added.len() >= max_add {
            break;
        }
        let u = sym.trim().to_uppercase();
        if u.is_empty()
            || rules.is_core_holding(&u)
            || rules.is_blocked_symbol(&u)
            || state.has_open_symbol(&u)
            || state
                .dynamic_watchlist
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&u))
        {
            continue;
        }
        while state.dynamic_watchlist.len() >= cap && !state.dynamic_watchlist.is_empty() {
            if let Some(idx) = state
                .dynamic_watchlist
                .iter()
                .position(|s| !state.has_open_symbol(s))
            {
                state.dynamic_watchlist.remove(idx);
            } else {
                break;
            }
        }
        if state.dynamic_watchlist.len() >= cap {
            break;
        }
        state.dynamic_watchlist.push(u.clone());
        added.push(u);
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::state::TraderState;
    use crate::rules::TraderRules;

    fn rules_with_cap(max_dynamic: u32, screened_top: u32) -> TraderRules {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.watchlists.max_dynamic_symbols = max_dynamic;
        rules.watchlists.screened.enabled = true;
        rules.watchlists.screened.top_n = screened_top;
        rules
    }

    #[test]
    fn capacity_uses_top_n_when_screened_enabled() {
        let rules = rules_with_cap(5, 12);
        assert_eq!(dynamic_watchlist_capacity(&rules), 12);
    }

    #[test]
    fn merge_replaces_prior_source_and_respects_cap() {
        let rules = rules_with_cap(5, 12);
        let mut state = TraderState::default();
        state.dynamic_watchlist = vec!["OLD1".into(), "KEEP".into()];
        state.fmp_dynamic_symbols = vec!["OLD1".into()];

        let prev = vec!["OLD1".to_string()];
        let added = merge_dynamic_symbols(
            &mut state,
            &rules,
            &prev,
            &["NEW1".into(), "NEW2".into()],
            12,
        );
        assert_eq!(added, vec!["NEW1", "NEW2"]);
        assert!(state.dynamic_watchlist.contains(&"KEEP".to_string()));
        assert!(state.dynamic_watchlist.contains(&"NEW1".to_string()));
        assert!(state.dynamic_watchlist.contains(&"NEW2".to_string()));
        assert!(!state.dynamic_watchlist.contains(&"OLD1".to_string()));
    }
}
