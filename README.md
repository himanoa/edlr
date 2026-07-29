# edlr — EliteDangerousLogRouter

**edlr** watches the Elite Dangerous Journal and `Status.json`, and routes game
events to sandboxed WASM plugins — a lightweight, loosely-coupled alternative to
the all-in-one EDMC-style companion apps.

Plugins run inside a wasmtime sandbox with no ambient access to the network,
filesystem, or processes. Everything a plugin can touch is declared in its
manifest and individually approved by the user through the UI.

## Features

- **Journal tailing** — inotify + polling, JSON Lines parsing, `Status.json`
  monitoring. Read position is persisted across restarts, so events are neither
  replayed nor lost when the daemon restarts
- **WASM plugin system** — plugins are WebAssembly components (Rust, Go/TinyGo,
  or anything targeting `wasm32-wasip2`) driven on dedicated threads
- **Capability-based security** — plugins declare what they need
  (HTTP hosts, sidecar processes, filesystem roots) and get nothing until the
  user approves each request. Approvals are invalidated automatically when a
  manifest changes what it asks for
- **Sidecar processes** — plugins can request native helper processes
  (e.g. a TTS engine); the executable path is always chosen by the user, never
  by the plugin
- **Inter-plugin bus** — driver components mediate plugin-to-plugin
  communication through declared, user-approved topics
- **Scheduling** — plugins declare `[[schedule]]` entries (`interval-seconds`
  or a 5-field `cron` expression) and get called back through `on-schedule`;
  `on-stop` gives them a best-effort chance to flush on graceful shutdown
- **GUI client** — React SPA (Logs / Plugins / Dashboard / Settings) served in
  the browser or wrapped as a Tauri 2 desktop app, talking to the daemon over
  WebSocket

## Getting started

### Prerequisites

- Rust (stable) — the daemon is built with Cargo
- For the UI: Node.js + pnpm, and for the desktop app the Tauri 2 system
  dependencies (`libwebkit2gtk-4.1-dev` etc.)

### Run the daemon

```
cargo run -p edlr-core --bin edlr -- --journal-dir <path-to-journal-dir>
```

If `--journal-dir` is omitted, edlr looks for the default Proton journal path.
Events are streamed to stdout as one JSON object per line, and served over
WebSocket at `ws://127.0.0.1:8137/ws`.

See [docs/cli.md](docs/cli.md) for the full list of CLI flags.

### Run the UI

```
# Browser (development)
cd ui/frontend && pnpm install && pnpm dev   # http://localhost:5173

# Desktop app (Tauri)
cd ui/src-tauri && cargo tauri dev
```

The Tauri app spawns the daemon automatically if it is not already running.
See [docs/ui.md](docs/ui.md) for details.

### Write a plugin

Step-by-step tutorials take you from an empty directory to a plugin that
filters on settings, calls an HTTP API, runs on a schedule, and talks to a
driver you wrote yourself: [docs/plugin-tutorial-rust.md](docs/plugin-tutorial-rust.md)
(Rust) and [docs/plugin-tutorial-tinygo.md](docs/plugin-tutorial-tinygo.md)
(TinyGo). Elite Dangerous is not needed — the Journal is a text file you can
write yourself.

For a smaller taste, `examples/plugins/hello-logger` is a minimal plugin that
logs the events it subscribes to; build it, drop it into
`~/.config/edlr/plugins/`, and restart the daemon
([docs/plugins.md](docs/plugins.md#hello-logger-サンプルのビルドと配置)).

## Documentation

| Document | Contents |
| --- | --- |
| [docs/plugin-tutorial-rust.md](docs/plugin-tutorial-rust.md) | Tutorial: writing a plugin in Rust, from scratch to bus integration |
| [docs/plugin-tutorial-tinygo.md](docs/plugin-tutorial-tinygo.md) | The same tutorial in TinyGo |
| [docs/cli.md](docs/cli.md) | CLI flags, journal read-position persistence, the `replay` flag |
| [docs/plugins.md](docs/plugins.md) | Plugin system: WIT interface, `manifest.toml`, plugin layout, settings RPC |
| [docs/capabilities.md](docs/capabilities.md) | Capabilities and approval flow: HTTP (`driver-http`), sidecar processes (`driver-process`), filesystem access (`driver-fs`) |
| [docs/drivers.md](docs/drivers.md) | Inter-plugin bus: `driver.toml`, `[[bus]]` declarations, retained values, queue semantics |
| [docs/ui.md](docs/ui.md) | Running the UI, Tauri journal-directory settings |
| [spec.md](spec.md) | Design document (Japanese) |

Documentation under `docs/` is currently written in Japanese.

## Repository layout

- `core/` — the Rust kernel: journal tailing, event routing, plugin host
  (wasmtime), WebSocket/RPC server. Binary name: `edlr`
- `drivers/` — privileged driver layer (http / channel)
- `config/` — `edlr-config` crate: config-file path resolution and default
  Proton journal-path discovery, shared by the daemon and the Tauri app
- `ui/` — GUI client: `frontend/` (React + Vite SPA) and `src-tauri/`
  (thin Tauri 2 shell)
- `examples/` — sample plugins (`hello-logger`, `state-reader`,
  `inara-uploader`, `tutorial-jump-log-{rs,go}`) and drivers (`ed-state`,
  `tutorial-tracker-{rs,go}`)
- `scripts/` — `install-examples.sh` builds the bundled plugins/drivers and
  installs them into the daemon's `plugins-dir` / `drivers-dir`

## Status

edlr is under active development and has no stable release yet. The plugin ABI
(WIT package `edlr:plugin`, currently `@0.4.0`) is still evolving and breaks
between versions — plugins must be rebuilt against the current `core/wit` when
the version bumps.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
