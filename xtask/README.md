# xtask

Developer tasks, run as `cargo xtask <subcommand>` (alias in `.cargo/config.toml`):

| Subcommand | Purpose |
|---|---|
| `layering` | Check the crate dependency DAG against `docs/architecture.md` §3 (no cycles, strictly one-directional) |
| `coverage` | Run the test suite under `cargo-llvm-cov` and print a summary |
| `i18n-extract` | Extract translatable strings into `po/hatchery.pot` — **not implemented until M4** |
| `record-fixtures` | Record real provider SSE streams as test fixtures — **not implemented until M1** |

Unimplemented subcommands fail loudly with a pointer to the milestone that delivers them, rather
than silently doing nothing (ADR-0009).

This is a dev tool, not a product crate: `publish = false`.
