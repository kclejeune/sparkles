# Standalone desktop application

U13 is specified and unimplemented. [Roadmap item 746](../../ROADMAP.md)
orders the work after the shared shell and the initial workbench features.

The [design](SPEC.md) calls for a desktop application covering the existing web UI,
with local datasets and remote server connections. The initial application uses
Tauri or Electron to reuse the web UI while its design evolves. A later migration to
GPUI follows once the UI and its interactions stabilize, with measured performance
and parity checks before the cutover.

The first phase selects Tauri or Electron and defines local operation. The second
delivers the application, its packages and shared parity checks. The third migrates
the stable interface to GPUI. No phase
has shipped. The [visualization guide](../../visualization/README.md) describes the
interface available today.
