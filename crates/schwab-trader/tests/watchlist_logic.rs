use schwab_trader::rules::TraderRules;
use schwab_trader::watchlist::patch::apply_build_to_rules;
use schwab_trader::watchlist::WriteTarget;

#[test]
fn candidate_pool_excludes_core_and_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let pool_path = dir.path().join("pool.yaml");
    std::fs::write(
        &pool_path,
        "symbols:\n  - AAPL\n  - AMZN\n  - TQQQ\n",
    )
    .unwrap();
    let rules_path = dir.path().join("rules.yaml");
    std::fs::write(
        &rules_path,
        r#"version: 1
trader_id: test
accounts:
  - hash: ABC
    enabled: true
watchlists:
  core: [AAPL]
  candidate_pool_file: pool.yaml
playbook:
  style: swing
  filters:
    blocked_symbols: [TQQQ]
"#,
    )
    .unwrap();

    let rules = TraderRules::load(&rules_path).unwrap();
    let screen = rules.symbols_for_screening(&rules_path).unwrap();
    assert_eq!(screen, vec!["AMZN"]);
}

#[test]
fn apply_build_writes_thematic_only_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let rules_path = dir.path().join("rules.yaml");
    std::fs::write(
        &rules_path,
        "version: 1\ntrader_id: test\naccounts:\n  - hash: ABC\n    enabled: true\nwatchlists:\n  core: [AAPL]\n  thematic: []\n",
    )
    .unwrap();
    let mut rules = TraderRules::load(&rules_path).unwrap();
    let thematic = vec![schwab_trader::rules::WatchlistThematic {
        symbol: "JPM".into(),
        tags: vec!["screened".into()],
    }];
    apply_build_to_rules(&mut rules, &thematic, &["JPM".into()], WriteTarget::Thematic).unwrap();
    assert_eq!(rules.watchlists.thematic.len(), 1);
    assert_eq!(rules.watchlists.thematic[0].symbol, "JPM");
    assert_eq!(rules.watchlists.core, vec!["AAPL"]);
}

#[test]
fn pool_loads_from_rules_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("universe")).unwrap();
    std::fs::write(
        dir.path().join("universe/test.yaml"),
        "symbols:\n  - XOM\n",
    )
    .unwrap();
    let rules_path = dir.path().join("rules.yaml");
    std::fs::write(
        &rules_path,
        "version: 1\ntrader_id: t\naccounts:\n  - hash: ABC\n    enabled: true\nwatchlists:\n  candidate_pool_file: universe/test.yaml\n",
    )
    .unwrap();
    let rules = TraderRules::load(&rules_path).unwrap();
    let syms = rules.candidate_pool_symbols(&rules_path).unwrap();
    assert_eq!(syms, vec!["XOM"]);
}
