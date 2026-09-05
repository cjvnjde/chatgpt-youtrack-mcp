# Configuration

[Documentation home](../README.md#documentation)

The binary reads environment variables at startup. Docker Compose reads `.env`
for interpolation and passes only the variables declared in its service
`environment` sections. Start with [`.env.example`](../.env.example).

## Required deployment values

| Variable | Purpose |
|---|---|
| `YOUTRACK_URL` | YouTrack base URL, such as `https://my-items.youtrack.cloud`, without `/api`. Trailing slashes are removed. |
| `YOUTRACK_TOKEN` | Permanent YouTrack token. Its account permissions apply to every caller. |
| `MCP_AUTH_TOKEN` | Public MCP bearer secret; required in HTTP mode, at least 32 bytes with no whitespace. |
| `CONTROL_PLANE_API_KEY` | OpenAI control-plane credential used by the tunnel container. |
| `CONTROL_PLANE_TUNNEL_ID` | Identifier of the Secure MCP Tunnel used by the tunnel container. |

Create a permanent token in your YouTrack account's security settings with the
YouTrack scope and the permissions needed for your workflows. Generate a
separate MCP bearer token:

```sh
openssl rand -hex 32
```

Store the output in `MCP_AUTH_TOKEN`. Keep real credentials out of Git, client
examples, and shared logs. This repository has no root `.gitignore`; keep a local
`.env` excluded, for example by adding `.env` to `.git/info/exclude`, or use your
deployment platform's environment editor.

## Transport and logging

| Variable | Binary default | Compose behavior |
|---|---|---|
| `MCP_TRANSPORT` | `stdio` | Fixed to `http`. Supports `stdio` or `http`. |
| `MCP_HTTP_ADDR` | `0.0.0.0:8080` | Fixed to `0.0.0.0:8080`; bearer-protected `/mcp`. |
| `MCP_INTERNAL_ADDR` | Disabled | Fixed to `0.0.0.0:8081`; unauthenticated `/mcp`. |
| `RUST_LOG` | `info` | Set from `MCP_LOG_LEVEL`, default `info`. MCP logs go to stderr. |
| `LOG_LEVEL` | Tunnel setting | Passed to the tunnel; default `info`. |
| `LOG_FORMAT` | Tunnel setting | Passed to the tunnel; default `json`. Does not change MCP log formatting. |
| `TUNNEL_CLIENT_VERSION` | Build setting | Tunnel source tag; default `v0.0.10`. Requires rebuilding the image. |
| `APP_IMAGE` | Compose setting | Shared image name for both services; default `youtrack-mcp-tunnel:local`. |

Listen addresses must be IP socket addresses, not hostnames. The two addresses
must differ. An unset or empty `MCP_INTERNAL_ADDR` disables the internal listener
when running the binary directly. HTTP mode always requires `MCP_AUTH_TOKEN`.
Changing transport values only in `.env` does not override Compose's fixed values.

## Optional YouTrack settings

These are supported by the binary but **not forwarded by the checked-in Compose
file**. Add the settings you need to `youtrack-mcp.environment` in Compose, or
export them when running locally.

| Variable | Default | Purpose |
|---|---|---|
| `YOUTRACK_DEFAULT_PROJECT` | None | Expands bare issue numbers to `PROJ-123`. Prefer readable IDs such as `ABC-123`. |
| `YOUTRACK_TIMEZONE` | `Europe/Moscow` | IANA timezone for date conversion and report bucketing. Invalid values fall back to the default. |
| `YOUTRACK_HOLIDAYS` | Empty | Comma-separated `YYYY-MM-DD` dates excluded from expected workdays. Invalid entries are ignored. |
| `YOUTRACK_PRE_HOLIDAYS` | Empty | Comma-separated dates with a 420-minute expected day instead of 480 minutes. Invalid entries are ignored. |
| `YOUTRACK_USER_ALIASES` | Empty | Comma-separated `alias:login` pairs for user resolution. |
| `YOUTRACK_DOWNLOAD_DIR` | System temporary directory | Directory for attachment downloads that are not returned inline. |
| `YOUTRACK_OPENAPI_PATH` | None | Local OpenAPI JSON file instead of fetching `/api/openapi.json` at startup. |

For example, add `YOUTRACK_TIMEZONE: ${YOUTRACK_TIMEZONE:-Europe/Moscow}` to the
MCP service's environment mapping. For a pinned schema, also mount the JSON file
read-only and set `YOUTRACK_OPENAPI_PATH` to its **container** path. Pinning avoids
the startup schema fetch; tool calls still need access to YouTrack, and the
schema must match that instance.

## Direct HTTP client

After configuring the [reverse proxy](deployment.md#reverse-proxy-and-dokploy),
use a client that supports MCP Streamable HTTP:

```json
{
  "mcpServers": {
    "youtrack": {
      "type": "http",
      "url": "https://youtrack-mcp.example.com/mcp",
      "headers": {
        "Authorization": "Bearer YOUR_MCP_AUTH_TOKEN"
      }
    }
  }
}
```

The configuration shape varies by client. Preserve the endpoint and authorization
header. Missing or incorrect credentials return HTTP `401` with
`WWW-Authenticate: Bearer`.

## Local stdio client

Build the checked-in binary using the [development guide](development.md), then
configure a process-based MCP client:

```json
{
  "mcpServers": {
    "youtrack": {
      "command": "/absolute/path/to/youtrack-mcp/target/release/youtrack-mcp",
      "env": {
        "MCP_TRANSPORT": "stdio",
        "YOUTRACK_URL": "https://my-items.youtrack.cloud",
        "YOUTRACK_TOKEN": "perm-replace-me"
      }
    }
  }
}
```

Stdio needs neither a tunnel nor an MCP bearer token. Use this repository's build
when you need its behavior; installing an upstream release does not necessarily
include the changes checked in here.
