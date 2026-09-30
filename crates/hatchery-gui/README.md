# hatchery-gui

Native desktop frontend built on GTK4 + libadwaita (ADR-0008): session list, message stream with
collapsible reasoning, approval dialogs, diff views, branch timeline, rewind panel, prompt
viewer and preferences.

All state transformation lives in GTK-free view-models so it can be unit tested; GTK only
projects them. i18n uses gettext and RTL is handled by Pango.

Workspace layer **D** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/frontends.md](../../docs/design/frontends.md) §3 ·
Worklog: [docs/worklog/gui.md](../../docs/worklog/gui.md)
