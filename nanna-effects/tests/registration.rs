mod billing {
    nanna_effects::touches!("db.orders");
    nanna_effects::touches!("job.invoice_nightly");
}

mod auth {
    nanna_effects::touches!("db.users");
}

mod ledger {
    nanna_effects::touches!("db.ledger");
}

mod ledger_replica {
    nanna_effects::touches!("db.ledger");
}

#[test]
fn declarations_carry_asset_module_file_and_line() {
    let all = nanna_effects::declared();
    let orders = all.iter().find(|t| t.asset == "db.orders").unwrap();
    assert_eq!(orders.module_path, concat!(module_path!(), "::billing"));
    assert!(orders.file.ends_with("registration.rs"));
    assert_eq!(orders.line, 2);
}

#[test]
fn every_invocation_registers_once() {
    let all = nanna_effects::declared();
    for asset in ["db.orders", "job.invoice_nightly"] {
        assert_eq!(
            all.iter().filter(|t| t.asset == asset).count(),
            1,
            "{asset}"
        );
    }
}

#[test]
fn same_asset_from_two_sites_registers_both() {
    let sites: Vec<_> = nanna_effects::declared()
        .into_iter()
        .filter(|t| t.asset == "db.ledger")
        .collect();
    assert_eq!(sites.len(), 2);
    assert_ne!(sites[0].line, sites[1].line);
}
