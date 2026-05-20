# MCP

`origin-mcp` is a Model Context Protocol v1.29 client. Servers can be local
processes (stdio transport), HTTP endpoints, or SSE streams. Tools exposed by
MCP servers land in the same compile-time tool registry as native tools, so
they go through the same permission tiering, speculative-dispatch rules, and
CAS handling.

## Registering a server

```bash
# Local stdio server (default transport)
origin mcp add github-mcp -- npx -y @modelcontextprotocol/server-github

# HTTP server
origin mcp add internal-search --transport http --url https://mcp.acme.corp

# SSE server
origin mcp add events --transport sse --url https://events.example.com/mcp
```

Each `add` records the server in `config.toml` under `[mcp.servers.<name>]`
and registers its tools with the prefix `mcp:<server>:<tool>`. Removing is
symmetric:

```bash
origin mcp remove github-mcp
origin mcp list
origin mcp status        # connection state + last heartbeat per server
```

## Configuration

A registered server entry looks like this in `config.toml`:

```toml
[mcp.servers.github-mcp]
transport = "stdio"
command   = "npx"
args      = ["-y", "@modelcontextprotocol/server-github"]
env       = { GITHUB_PERSONAL_ACCESS_TOKEN = "@keyring:github-mcp" }

# Default permission tier for tools from this server. Per-tool overrides go
# under [mcp.servers.github-mcp.tools.<name>].
tier = "RequiresPermission"
urgency = "Medium"

# Quarantine policy. Servers that miss N consecutive heartbeats are pulled
# out of the registry; tools are marked Unavailable with a user-visible
# message instead of crashing the agent loop.
heartbeat_interval_ms = 5000
heartbeat_misses_to_quarantine = 3
```

The `@keyring:github-mcp` syntax pulls a credential from `origin-keyvault` at
runtime — never embed a token in `config.toml` directly. For OAuth-protected
HTTP/SSE servers, `origin mcp login <server>` runs a PKCE or device-flow
exchange and stores the refresh token in KeyVault. Background refresh keeps
the access token live without prompting the user mid-session.

## Connection-per-server backpressure

`origin-mcp` holds one persistent connection per server and multiplexes tool
calls over it by request ID. Each connection has a credit budget: callers
consume credits when emitting requests; the receiver issues credits as it
drains. A misbehaving server that stops responding stops getting requests —
queued calls fail fast with `McpUnavailable` rather than ballooning the
daemon's memory or stalling the agent loop.

A health monitor pings each server at the configured `heartbeat_interval_ms`.
On `heartbeat_misses_to_quarantine` consecutive misses the server moves to a
`Quarantined` state; its tools are still listed in the registry but reject
calls with a structured error the model can recover from (it just stops
trying to use them for the rest of the turn). When heartbeats resume the
server is automatically un-quarantined on the next pass.

## Permissions and sandboxing

MCP tools land in the same tier system as native tools:

- Read-style tools (anything matching `read*` / `list*` / `get*`) default to
  the `RequiresPermission / Low` tier and become eligible for
  speculative-dispatch alongside `Read` and `Grep`.
- Write-style tools default to `RequiresPermission / Medium`.
- Per-server overrides in `[mcp.servers.<name>.tier]` and per-tool overrides
  in `[mcp.servers.<name>.tools.<tool>]` let you opt into auto-allow for
  trusted servers.

MCP responses are validated against the registered schema at the IPC buffer
layer with a hard 16MB cap per response (configurable per server). Schema
mismatches are rejected before the agent loop ever sees them — the model
gets a structured error it can recover from rather than malformed bytes.

For diagnosing connection problems and quarantine reasons see
[Troubleshooting](troubleshooting.md).
