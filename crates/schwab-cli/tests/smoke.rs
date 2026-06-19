use schwab_cli::capabilities;

#[test]
fn capabilities_has_trader_commands() {
    let cmds = capabilities::all_commands();
    assert!(cmds.iter().any(|c| c.path == "accounts numbers"));
    assert!(cmds.iter().any(|c| c.path == "portfolio summary"));
    assert!(cmds.iter().any(|c| c.path == "trade buy" && c.mutation));
    assert!(cmds.iter().any(|c| c.path == "orders place" && c.mutation));
}
