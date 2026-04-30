# 🌌 arRPC Rust

[![Build prerelease](https://github.com/lekoOwO/arrpc/actions/workflows/prerelease.yml/badge.svg)](https://github.com/lekoOwO/arrpc/actions/workflows/prerelease.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](https://opensource.org/licenses/GPL-3.0)

A high-performance, modern rewrite of **arRPC** in Rust. 🚀

**arRPC** is a Discord-compatible Rich Presence server that allows you to show what you're doing on Discord without needing the official client running, or while using Discord in a browser. This Rust rewrite focuses on extreme efficiency, memory safety, and a "set and forget" experience.

---

## ✨ Key Features

- 🔌 **Fully Discord-Compatible**: Implements both WebSocket and IPC (Unix Sockets & Named Pipes) transports.
- 🌉 **Browser Bridge**: Seamlessly connect browser-based extensions or alternative clients.
- 🔍 **Automatic Game Detection**: Scans your running processes and matches them against Discord's official database.
- ⚡ **Lightweight & Fast**: Built with Rust for minimal CPU and memory footprint.
- 🛡️ **Robust & Bounded**: Uses bounded queues to ensure slow clients never cause memory leaks.
- 💻 **Cross-Platform**: Full support for Windows, macOS, and Linux.

---

## 🚀 Quick Start

### Installation

Download the latest binary from the [Releases](https://github.com/lekoOwO/arrpc/releases) page, or build it from source:

```bash
# Clone the repository
git clone https://github.com/lekoOwO/arrpc-rust.git
cd arrpc

# Build and run
cargo run --release
```

### Basic Usage

Simply run the binary, and it will start listening for RPC connections:
- **WebSocket RPC**: `127.0.0.1:6463` (with fallbacks)
- **IPC Server**: `discord-ipc-0` (standard path)
- **Bridge Server**: `127.0.0.1:1337`

---

## ⚙️ Configuration

arRPC is highly configurable via environment variables or command-line arguments.

### Common Options

| Environment Variable | CLI Argument | Description | Default |
| :--- | :--- | :--- | :--- |
| `ARRPC_DEBUG` | `-d` / `--debug` | Enable verbose diagnostic logging | `0` |
| `ARRPC_NO_PROCESS_SCANNING` | `--no-process-scanning` | Disable automatic game detection | `0` |
| `ARRPC_BRIDGE_PORT` | `--bridge-port` | Change the bridge WebSocket port | `1337` |
| `ARRPC_DATA_DIR` | `--data-dir` | Path to store/load database files | (app data) |

### Inspection Tools

```bash
# List all games in the embedded database
arrpc --list-database

# List currently detected games
arrpc --list-detected

# Update the local detectable database
arrpc update-db
```

---

## 🛠️ Development

We welcome contributions! To get started:

```bash
# Run unit tests
cargo test

# Check formatting and linting
cargo fmt --check
cargo clippy
```

---

## 📜 Credits & License

- Original project: [Creationsss/arrpc-bun](https://github.com/Creationsss/arrpc-bun)
- License: [GPL-3.0](LICENSE)

---

<p align="center">
  Made with ❤️ for the Discord community.
</p>
