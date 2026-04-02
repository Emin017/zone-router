# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
cargo build --release

# Run all tests
cargo test

# Run a single test by name
cargo test test_name

# Run tests in a specific file
cargo test --test acceptance
cargo test --test proxy_integration

# Lint
cargo clippy

# Format
cargo fmt

# Nix build
nix build
```

## Architecture

ZoneRouter is a single-binary Rust application with two concurrent subsystems sharing state via `Arc<RwLock<AppState>>`:

- **Proxy server** (`src/proxy/`) — axum HTTP server that accepts all routes via a fallback handler. On each request: validates the `x-api-key` header against `local_token`, swaps it for the active backend's real token, forwards the request via reqwest. SSE streaming responses are passed through using `LogOnDropStream`, which fires stats recording when the stream is dropped (i.e. when the client disconnects or the response finishes).

- **TUI** (`src/tui/`) — ratatui terminal UI running on a blocking thread (`spawn_blocking`). Polls crossterm events at ~30fps. `TuiState` holds ephemeral UI state (cursor, input mode, scroll). `InputMode` is a state machine for multi-step flows like adding/editing backends.

**Startup sequence** (`src/main.rs`): bind TCP listener → create shared state → spawn proxy task → run TUI on blocking thread → on TUI exit, call `force_shutdown` which sets `state.shutdown = true` (causes in-flight requests to drain) then waits up to 5s before aborting.

**State** (`src/state.rs`): `AppState` owns the config, active backend index, local token, and stats. Every mutation (switch, add, remove, edit backend) calls `persist_config()` which writes the full config to disk atomically. The local token is auto-generated as `sk-local-<uuid>` on first run and persisted.

**Config** (`src/config.rs`): TOML at `~/.config/zone-router/config.toml`. The `active` field on backends is derived from `active_index` at save time — only one backend is active at a time.

**Stats** (`src/stats.rs`): in-memory only, capped at 1000 log entries (`VecDeque`), with per-backend aggregates in a `HashMap`.

## Development Guidelines

- Follow Rust best practices: prefer `?` over `unwrap` in fallible paths, use `clippy` to catch idiom violations, keep `unsafe` absent.
- Favour functional and point-free style: chain iterator adapters (`.map`, `.filter`, `.filter_map`, `.fold`) over imperative loops, pass functions by reference rather than writing inline closures where a function already exists, and avoid intermediate mutable bindings when a pipeline reads clearly without them.
- All cargo commands (`cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`) must be run inside the Nix dev shell — enter it once with `nix develop` before running any toolchain commands.

## Testing

Tests are integration/acceptance level — they build real axum routers with `build_router()` and hit them with `tower::ServiceExt`. No mocking of the database or HTTP layer. `tests/common/mod.rs` has the shared state builder. `tests/acceptance.rs` covers the full acceptance criteria including the binary via `CARGO_BIN_EXE_zone-router`.
