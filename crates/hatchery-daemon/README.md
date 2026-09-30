# hatchery-daemon

The runtime host and the only writer of the database (ADR-0001, ADR-0002): single-instance
discovery, UDS/stdio listeners, the session manager with leases and generation numbers, the live
hub that fans events out to every attached frontend, and profile-based assembly with a fail-loud
startup audit (ADR-0009).

Sessions outlive frontends — closing a terminal does not stop a running turn.

Workspace layer **L3** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/daemon.md](../../docs/design/daemon.md) ·
Worklog: [docs/worklog/daemon.md](../../docs/worklog/daemon.md)
