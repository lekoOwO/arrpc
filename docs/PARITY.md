# arrpc-bun Replacement Parity

This document tracks the Rust rewrite against `Creationsss/arrpc-bun`.

## Implemented

- WebSocket RPC transport on `127.0.0.1:6463-6472`
- WebSocket Hyper-V fallback range on `60100-60120`
- IPC RPC transport on `discord-ipc-0` through `discord-ipc-9`
- Bridge WebSocket transport on `127.0.0.1:1337-1347`
- Bridge Hyper-V fallback range on `60000-60020`
- `ARRPC_DEBUG`
- `ARRPC_NO_BRIDGE`
- `ARRPC_BRIDGE_PORT`
- `ARRPC_BRIDGE_HOST`
- `ARRPC_WEBSOCKET_HOST`
- `ARRPC_NO_PROCESS_SCANNING`
- `ARRPC_NO_STEAM`
- `ARRPC_STATE_FILE`
- `ARRPC_IGNORE_LIST_FILE`
- `ARRPC_PARENT_MONITOR`
- `ARRPC_DATA_DIR`
- `--no-process-scanning`
- `--list-database`
- `--list-detected`
- `update-db` / `--update-db`
- `validate-fixes` / `--validate-fixes`
- READY dispatch payload
- `CONNECTIONS_CALLBACK`
- `SET_ACTIVITY`
- `INVITE_BROWSER`
- `GUILD_TEMPLATE_BROWSER`
- `DEEP_LINK`
- Activity clear on transport disconnect
- Button metadata translation
- Timestamp seconds-to-milliseconds normalization
- Process scanning on Windows through PowerShell CIM
- Process scanning on Linux through `/proc`
- Process scanning on macOS through `ps`
- Ignore list filtering by application id, executable name, and game name
- State file export using `/tmp/arrpc-state-{0-9}` style paths
- State file server endpoint metadata
- Detectable application database embedded at compile time
- Custom `detectable.json` through `ARRPC_DATA_DIR`
- Custom `detectable_fixes.json` through `ARRPC_DATA_DIR`
- Steam library detection and app manifest lookup
- Discord application name lookup from the detectable database
- Discord application name network fallback through
  `GET /api/v10/applications/{id}/rpc`
- arrpc-bun executable matching semantics:
  - `.app_name` markers for Steam-resolved games
  - launcher and non-launcher executable passes
  - strict argument matching followed by loose non-exact matching
  - missing custom-fix names default to `Custom Game`
- Non-overlapping process scan loop
- Bounded bridge client queues
- Pre-release builds on push to `rust-rewrite` for:
  - Linux x64
  - Linux ARM64
  - Linux x64 musl
  - Linux ARM64 musl
  - macOS Intel
  - macOS Apple Silicon
  - Windows x64

## Remaining arrpc-bun Runtime Gaps

- Windows process enumeration uses PowerShell CIM plus local command-line
  parsing instead of arrpc-bun's native Toolhelp/NT FFI scanner. This avoids
  WMIC, but it is not yet an implementation-identical replacement for
  arrpc-bun's failed-open cache and native yielding behavior.
- Steam lookup implements library and app manifest matching, but does not yet
  include arrpc-bun's resolved-path cache eviction behavior.
- Process matching behavior is aligned with arrpc-bun, but the Rust scanner
  currently scans the loaded database directly instead of maintaining
  arrpc-bun's executable index and scan-result cache. This is a performance
  difference, not an intended behavior difference.

## Remaining Packaging Gaps

- npm package distribution compatible with `bun install -g arrpc-bun`.
- AUR packaging metadata.

## Unsupported RPC Feasibility

arrpc-bun also marks these unsupported. Full support is possible only with a
Discord-authenticated host/client layer, not from the local RPC server alone:

- `AUTHORIZE` and `AUTHENTICATE` require OAuth/session handling and a token
  lifecycle.
- Guild, channel, and message queries require authenticated Discord API access
  and permission-aware response shaping.
- Voice/text channel control requires a live Discord client session to apply
  local client state changes.
- `SUBSCRIBE`/`UNSUBSCRIBE` require an event source and event routing model.
- `CAPTURE_SHORTCUT` and overlay integration require OS/client integration
  outside the RPC transport.

For a drop-in replacement, these commands should keep arrpc-bun's current
unsupported behavior unless a concrete consumer requires more. Real
implementation should be gated behind an authenticated host integration, because
adding synthetic protocol errors would itself diverge from arrpc-bun.

## Not Implemented Because arrpc-bun Also Marks Them Unsupported

- OAuth2 authorization and authentication commands.
- Guild/channel query commands.
- Voice and text channel control.
- Event subscription commands.
- Keyboard shortcut capture.
- Overlay integration.

## Behavioral Notes

- The Rust rewrite treats invite, guild-template, and deep-link as built-in
  protocol commands, matching the shape exposed by `arrpc-bun` rather than the
  original JavaScript embedding API.
- IPC and WebSocket transports share `RpcSession`, so command translation should
  remain identical across transports.
- The Rust bridge intentionally drops slow bridge clients once their queue fills
  instead of buffering indefinitely.
