//! The layering contract as a test, so `cargo nextest run` catches violations without anyone
//! having to remember `cargo xtask layering`.

#[test]
fn workspace_layering_contract_holds() {
    let report =
        xtask::layering::check().expect("layering contract violated; see docs/architecture.md §3");
    assert_eq!(
        report.members, 12,
        "expected 12 workspace members (11 crates + xtask); update docs/architecture.md §3, \
         docs/roadmap.md M0 and xtask::layering::LAYERS together"
    );
    assert!(
        report.edges >= 10,
        "only {} dependency edges — the scaffold probably lost its internal path dependencies",
        report.edges
    );
}
