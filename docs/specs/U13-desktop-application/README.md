# Standalone desktop application

U13 is specified and unimplemented. [Roadmap item 746](../../ROADMAP.md)
orders the work after the shared shell and the initial workbench features.

The [design](SPEC.md) calls for a desktop application covering the existing web UI,
with local datasets and remote server connections. GPUI is the preferred direction,
subject to measured performance and the cost of maintaining parity. Tauri and Electron
remain alternatives if reusing the existing interface better prevents drift.

The first phase chooses the architecture through a representative prototype. The
second delivers the application, its packages and shared parity checks. Neither phase
has shipped. The [visualization guide](../../visualization/README.md) describes the
interface available today.
