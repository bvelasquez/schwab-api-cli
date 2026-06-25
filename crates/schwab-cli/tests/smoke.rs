use schwab_cli::capabilities;

#[test]
fn capabilities_has_trader_commands() {
    let cmds = capabilities::all_commands();
    assert!(cmds.iter().any(|c| c.path == "accounts numbers"));
    assert!(cmds.iter().any(|c| c.path == "portfolio summary"));
    assert!(cmds.iter().any(|c| c.path == "portfolio buying-power"));
    assert!(cmds.iter().any(|c| c.path == "market info"));
    assert!(cmds.iter().any(|c| c.path == "market quotes"));
    assert!(cmds.iter().any(|c| c.path == "market hours"));
    assert!(cmds.iter().any(|c| c.path == "trade buy" && c.mutation));
    assert!(cmds.iter().any(|c| c.path == "orders schema"));
    assert!(cmds.iter().any(|c| c.path == "orders validate"));
    assert!(cmds.iter().any(|c| c.path == "orders place" && c.mutation));
    assert!(cmds.iter().any(|c| c.path == "options chain"));
    assert!(cmds.iter().any(|c| c.path == "options open" && c.mutation));
    assert!(cmds.iter().any(|c| c.path == "agent run" && c.mutation));
    assert!(cmds.iter().any(|c| c.path == "agent stop" && c.mutation));
    assert!(cmds.iter().any(|c| c.path == "disclaimer show"));
    assert!(cmds.iter().any(|c| c.path == "disclaimer accept" && c.mutation));
}
