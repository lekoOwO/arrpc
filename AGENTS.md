# Agent Development Guide

## Project Shape

This branch is an orphan Rust rewrite of arRPC. Treat it as a Rust project, not
as an incremental edit of the JavaScript branch.

Core files:

- `src/main.rs` wires bridge, IPC, WebSocket RPC, and process scanning together.
- `src/config.rs` owns the arrpc-bun compatible env var and CLI surface.
- `src/cli.rs` owns `--list-database`, `--list-detected`, `update-db`, and
  `validate-fixes`.
- `src/rpc.rs` owns RPC command translation and shared session state.
- `src/ipc.rs` owns Discord IPC framing and platform socket setup.
- `src/bridge.rs` owns the browser bridge and replay cache.
- `src/process_scan.rs` owns process enumeration and detectable app matching.
- `src/state_file.rs` owns `/tmp/arrpc-state-{0-9}` compatible state export.
- `src/ignore_list.rs` owns ignore list matching.
- `assets/detectable.json` is embedded with `include_str!`.

## Invariants

- WebSocket and IPC command behavior must go through `RpcSession`.
- Keep transport framing separate from RPC command translation.
- Never add unbounded queues or maps for client data.
- Bridge state must delete entries when `activity` is `null`.
- Process scans must not overlap.
- Keep the `rust-rewrite` branch buildable on Windows, Linux, and macOS.
- Treat `Creationsss/arrpc-bun` as the replacement target when deciding parity.
- When adding an `arrpc-bun` env var, update `docs/PARITY.md` and add a config
  parser test.

## Verification

Run these before committing:

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --all-features
cargo build --release --locked
```

On Windows PowerShell in this workspace, cargo may not be on `PATH`; use:

```powershell
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
& $cargo test --locked
```

## Adding Runtime Behavior

1. Add a focused unit test first.
2. Prefer testing pure command translation in `src/rpc.rs`.
3. Add transport tests in `src/ipc.rs` or `src/bridge.rs` only when framing or
   queue behavior changes.
4. Keep platform-specific code behind `#[cfg(...)]`.
5. Update `docs/PARITY.md` when behavior changes.

## GitHub Actions

`.github/workflows/prerelease.yml` builds Linux, Linux musl, macOS, and Windows
artifacts on push to `rust-rewrite`, uploads them as artifacts, moves the
`rust-rewrite-prerelease` tag, and updates the matching GitHub prerelease.
