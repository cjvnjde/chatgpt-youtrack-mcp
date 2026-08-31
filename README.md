# YouTrack MCP for ChatGPT and remote agents

Runs one typed YouTrack MCP server behind two clients:

```text
ChatGPT ── OpenAI Secure MCP Tunnel ── private HTTP, no auth ──► :8081/mcp ──┐
                                                                            ├──► YouTrack
Local agents ── public HTTPS + Bearer ────────────────────────► :8080/mcp ──┘
```

The OpenAI tunnel and external agents reach the same MCP process, so they see
the same tools and YouTrack data. They use separate listeners: port `8081` is
unauthenticated and reachable only on the private Compose network; port `8080`
requires the bearer token and is the only listener routed publicly. Traefik,
Caddy or Nginx terminates public TLS for direct agents.

The checked-in `youtrack-mcp/` source provides 17 curated workflow tools plus a
typed `api_*` tool for every operation advertised by the connected YouTrack
instance.

## Required environment variables

```env
CONTROL_PLANE_API_KEY=sk-replace-me
CONTROL_PLANE_TUNNEL_ID=tunnel_replace_me
YOUTRACK_URL=https://my-items.youtrack.cloud
YOUTRACK_TOKEN=perm-replace-me
MCP_AUTH_TOKEN=replace-with-a-random-64-character-hex-token
```

Generate the MCP bearer token instead of inventing one:

```bash
openssl rand -hex 32
```

`MCP_AUTH_TOKEN` must contain at least 32 bytes and no whitespace. It protects
only the public listener; the private OpenAI tunnel does not receive or use it.
Never commit the real control-plane key, YouTrack token or MCP bearer token.

Optional settings:

```env
TUNNEL_CLIENT_VERSION=v0.0.10
LOG_LEVEL=info
LOG_FORMAT=json
MCP_LOG_LEVEL=info
```

## Dokploy deployment

1. Create a Dokploy project and Compose service.
2. Select this repository and `docker-compose.yml`.
3. Add the required environment variables.
4. Deploy both Compose services.
5. Attach a domain such as `youtrack-mcp.example.com` to the
   `youtrack-mcp` service on port `8080`.
6. Enable HTTPS in Dokploy/Traefik.
7. Configure the public router to forward only port `8080` and `/mcp`
   (including `/mcp/...`). Never publish internal port `8081`, the tunnel
   health UI or any unrelated container route.
8. Add rate limiting at the reverse proxy.

No Docker host port is published. The reverse proxy reaches the service over
the Compose network, while the `openai-tunnel` service uses:

```text
http://youtrack-mcp:8081/mcp
```

Port `8081` has no MCP authentication. Its only boundary is the private Compose
network, and it must never be attached to a public domain or host port.

## Connect any Streamable HTTP MCP client

Point the client at the public HTTPS endpoint and send the bearer token:

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

The client must support MCP Streamable HTTP. A missing or incorrect token gets
HTTP `401` with `WWW-Authenticate: Bearer`.

## OpenAI tunnel

Create a Secure MCP Tunnel in the OpenAI Platform and set:

```env
CONTROL_PLANE_API_KEY=...
CONTROL_PLANE_TUNNEL_ID=tunnel_...
```

The `openai-tunnel` Compose service connects without a bearer token through the
private `http://youtrack-mcp:8081/mcp` listener. It does not use the public
domain or public TLS.

After deployment:

1. Enable Developer mode in ChatGPT.
2. Create a developer-mode app/plugin.
3. Choose **Tunnel** as the connection type.
4. Select `CONTROL_PLANE_TUNNEL_ID`.
5. Scan/discover the YouTrack tools.

## Binary transport modes

The `youtrack-mcp` binary keeps stdio as its default for local process-based
clients:

```text
MCP_TRANSPORT=stdio
```

The Compose deployment selects HTTP with separate public and tunnel listeners:

```env
MCP_TRANSPORT=http
MCP_HTTP_ADDR=0.0.0.0:8080
MCP_INTERNAL_ADDR=0.0.0.0:8081
MCP_AUTH_TOKEN=<32+ byte secret>
```

`MCP_HTTP_ADDR` is bearer-protected. `MCP_INTERNAL_ADDR` is optional,
unauthenticated and intended only for a private container network.

TLS belongs at the reverse proxy. The MCP process intentionally serves plain
HTTP only on the private container network.

## Security boundary

- Use HTTPS for every public connection.
- Keep `MCP_AUTH_TOKEN` random, private and at least 32 bytes.
- Configure the reverse proxy to preserve the `Authorization` header.
- Rate-limit `/mcp` at the reverse proxy.
- Expose only public port `8080` and `/mcp`; never expose internal port `8081`,
  the tunnel UI or YouTrack credentials.
- Rotate the public token by changing `MCP_AUTH_TOKEN` and redeploying the MCP
  service.
- YouTrack permissions granted to `YOUTRACK_TOKEN` govern every MCP caller.

The bearer token authenticates callers to the MCP. It does not create
per-caller YouTrack identities: every caller operates with the configured
`YOUTRACK_TOKEN`.

## Health checks

The MCP container exposes private health endpoints:

```bash
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8080/readyz
```

Expected responses are `live` and `ready`.

The tunnel container separately exposes its operator endpoints on its own
loopback port `8080`:

```bash
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8080/readyz
curl -fsS http://127.0.0.1:8080/api/status
```

## Updating

Update the checked-in source under `youtrack-mcp/`, run its tests and redeploy.
The Docker image builds both the MCP binary and pinned tunnel client from
source.
