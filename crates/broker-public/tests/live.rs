use broker_core::{
    AccountRef, AssetKind, BrokerKind, LegInstruction, NetPrice, OrderIntent, Session, SpreadKind,
    SpreadLeg, TimeInForce,
};
use broker_public::PublicBackend;

fn load_env() {
    let candidates = [
        std::path::PathBuf::from(".env"),
        std::path::PathBuf::from("../.env"),
        std::path::PathBuf::from("../../.env"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if std::env::var(key).is_err() {
                std::env::set_var(key, value.trim().trim_matches('"'));
            }
        }
        break;
    }
}

#[tokio::test]
#[ignore = "calls the live Public API with PUBLIC_API_KEY"]
async fn live_readonly() {
    load_env();
    let backend = PublicBackend::from_env().expect("PUBLIC_API_KEY");
    let (trading, data) = backend.into_arcs();
    let accounts = trading.accounts().await.expect("accounts");
    assert!(!accounts.is_empty(), "expected at least one Public account");
    let account = accounts
        .into_iter()
        .find(|account| account.broker == BrokerKind::Public)
        .expect("public account");
    let portfolio = trading.portfolio(&account).await.expect("portfolio");
    assert!(portfolio.balances.equity >= 0.0 || portfolio.balances.cash_buying_power >= 0.0);
    let quotes = data
        .quotes(&account, &["AAPL".into()])
        .await
        .expect("quotes");
    assert!(
        quotes.iter().any(|quote| quote.last.is_some() || quote.bid.is_some()),
        "AAPL quote missing prices"
    );
    let expirations = data
        .option_expirations(&account, "AAPL")
        .await
        .expect("expirations");
    assert!(!expirations.is_empty(), "AAPL expirations empty");
    let chain = data
        .option_chain(&account, "AAPL", &expirations[0])
        .await
        .expect("chain");
    assert!(!chain.contracts.is_empty(), "chain had no contracts");
    let puts: Vec<_> = chain
        .contracts
        .iter()
        .filter(|contract| contract.put_call.eq_ignore_ascii_case("P"))
        .take(2)
        .collect();
    if puts.len() == 2 {
        let quote = data
            .strategy_quote(
                &account,
                &[
                    SpreadLeg {
                        instruction: LegInstruction::SellToOpen,
                        symbol: puts[0].symbol.clone(),
                        asset: AssetKind::Option,
                        ratio: 1.0,
                    },
                    SpreadLeg {
                        instruction: LegInstruction::BuyToOpen,
                        symbol: puts[1].symbol.clone(),
                        asset: AssetKind::Option,
                        ratio: 1.0,
                    },
                ],
            )
            .await
            .expect("strategy quote");
        assert!(
            quote.bid.is_some() || quote.ask.is_some() || quote.last.is_some(),
            "strategy quote had no price"
        );
    }
    let mut priced: Vec<_> = chain
        .contracts
        .iter()
        .filter(|contract| contract.put_call.eq_ignore_ascii_case("P") && contract.bid.unwrap_or(0.0) > 0.0)
        .collect();
    priced.sort_by(|left, right| left.strike.total_cmp(&right.strike));
    let mut spread = None;
    for (index, long) in priced.iter().enumerate() {
        if let Some(short) = priced[index + 1..].iter().find(|short| {
            let width = short.strike - long.strike;
            width > 0.0 && width <= 5.0
        }) {
            let credit = (short.bid.unwrap_or(0.0) - long.ask.unwrap_or(0.0)).max(0.01);
            spread = Some((short.symbol.clone(), long.symbol.clone(), credit));
            break;
        }
    }
    let (short, long, credit) = spread.expect("no priced put spread to preflight");
    {
        let intent = OrderIntent::Spread {
            strategy: SpreadKind::Vertical,
            legs: vec![
                SpreadLeg {
                    instruction: LegInstruction::SellToOpen,
                    symbol: short,
                    asset: AssetKind::Option,
                    ratio: 1.0,
                },
                SpreadLeg {
                    instruction: LegInstruction::BuyToOpen,
                    symbol: long,
                    asset: AssetKind::Option,
                    ratio: 1.0,
                },
            ],
            contracts: 1.0,
            net: NetPrice::Credit(credit),
            tif: TimeInForce::Day,
            session: Session::Core,
        };
        let preflight = trading.preflight(&account, &intent).await.expect("spread preflight");
        assert!(
            preflight.accepted,
            "spread preflight rejected: {}",
            preflight.message
        );
    }
    let _ = AccountRef::new(BrokerKind::Public, account.id);
}
