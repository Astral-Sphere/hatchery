# hatchery-daemon

The runtime host and the only writer of the database (ADR-0001, ADR-0002): single-instance
discovery, UDS/stdio listeners, the session manager with leases and generation numbers, the live
hub that fans events out to every attached frontend, and profile-based assembly with a fail-loud
startup audit (ADR-0009).

Sessions outlive frontends — closing a terminal does not stop a running turn.

This is also where the shadow-git checkpoint *policy* lives (M2 Phase 1): the budget ladder and the
orphan sweep both need the `checkpoints` table and the shadow repository in one place, and those two
are siblings at L2, which may not depend on each other.

Workspace layer **L4** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/daemon.md](../../docs/design/daemon.md) ·
Worklog: [docs/worklog/daemon.md](../../docs/worklog/daemon.md)
