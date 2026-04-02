# ZoneRouter

Local HTTP proxy that routes Claude Code API requests to multiple backends. Switch backends on the fly via a TUI with real-time request logs and stats.

> The name is a nod to the Zone in Andrei Tarkovsky's *Stalker* (1979).

## Quick Start

```bash
nix develop
cargo build --release

# Launch (auto-generates config and local token on first run)
zone-router

# Configure Claude Code
eval $(zone-router env)
claude
```

## Configuration

Default path: `~/.config/zone-router/config.toml`

```toml
[proxy]
listen = "127.0.0.1:8080"
local_token = ""  # leave empty to auto-generate

[[backends]]
name = "main-api"
url = "https://api.anthropic.com"
token = "sk-ant-xxx"
active = true

[[backends]]
name = "backup"
url = "https://backup.example.com"
token = "sk-xxx"
active = false
```

## CLI

```
zone-router [OPTIONS] [COMMAND]

Commands:
  env    Print export statements for Claude Code integration

Options:
  --port <PORT>      Override listen port (not persisted to config)
  --config <PATH>    Use a custom config file path
```

## TUI Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate up/down |
| `Enter` / `1-9` | Switch backend |
| `a` | Add backend |
| `e` | Edit backend |
| `d` | Delete backend |
| `t` | Show local token |
| `Tab` | Toggle panel focus |
| `G` / `gg` | Jump to bottom/top |
| `/` | Search |
| `q` | Quit |

## How It Works

```
Claude Code  -->  zone-router (localhost:8080)  -->  Backend API
             x-api-key: local_token           x-api-key: backend_token
```

The proxy validates the local token, swaps it for the active backend's real token, and forwards the request. SSE streaming responses are passed through chunk by chunk. In-flight requests are unaffected when switching backends.
