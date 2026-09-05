# Deployment

[Documentation home](../README.md#documentation)

The supplied [Dockerfile](../Dockerfile) builds the checked-in Rust MCP server and
a pinned OpenAI tunnel client into one image. [Compose](../docker-compose.yml)
runs a separate container for each binary, as a non-root user with UID `10001`.

## Start the stack

You need Docker with Compose, a reachable YouTrack instance and permanent token,
and OpenAI tunnel credentials. The build needs network access to base-image
registries, Debian packages, GitHub, and Rust dependencies. Runtime needs access
to YouTrack and the tunnel service; uploads also need the temporary file URL.

From the repository root:

```sh
cp .env.example .env
openssl rand -hex 32
```

Fill in `.env` with the generated `MCP_AUTH_TOKEN`, `YOUTRACK_URL`,
`YOUTRACK_TOKEN`, `CONTROL_PLANE_API_KEY`, and `CONTROL_PLANE_TUNNEL_ID`.
Keep the file excluded from Git as described in [configuration](configuration.md).
Then start and inspect the services:

```sh
docker compose up -d --build
docker compose ps
docker compose logs --tail=100 youtrack-mcp openai-tunnel
```

The MCP fetches the YouTrack schema before serving requests. Compose starts the
tunnel after the MCP health check succeeds. The tunnel targets
`http://youtrack-mcp:8081/mcp` without a bearer token. Configure your ChatGPT
tunnel connection to use the same tunnel ID, then refresh tool discovery.

## Reverse proxy and Dokploy

| Container endpoint | Intended use | Authentication |
|---|---|---|
| `youtrack-mcp:8080/mcp` | Direct clients through an HTTPS proxy | `Authorization: Bearer <MCP_AUTH_TOKEN>` |
| `youtrack-mcp:8081/mcp` | Private tunnel | None; private network is the boundary. |
| `youtrack-mcp:8080/healthz`, `/readyz` | Private health checks | None. |

Compose uses `expose`, not `ports`; it publishes no Docker host ports. A reverse
proxy must share a Docker network with the MCP service. `expose` does not enforce
access control between containers on that network.

For Dokploy:

1. Create a project and Compose service using this repository and `docker-compose.yml`.
2. Add the required environment variables and deploy both services.
3. Attach a domain, such as `youtrack-mcp.example.com`, to `youtrack-mcp` on port `8080`.
4. Enable HTTPS and route `/mcp`, including `/mcp/...`, to that port without stripping the prefix.
5. Preserve the `Authorization` header and MCP transport headers; allow Streamable HTTP traffic and streaming responses.
6. Add reverse-proxy rate limiting appropriate to your clients.

Traefik, Caddy, or Nginx can provide the same routing. TLS terminates at the
proxy; container listeners serve plain HTTP. Never publicly route port `8081`,
health endpoints, or the tunnel operator UI. Connect direct clients with the
[HTTP configuration example](configuration.md#direct-http-client).

For a deployment with direct clients only, build and start just the MCP service:

```sh
docker compose up -d --build youtrack-mcp
```

The checked-in configuration still enables the private listener. Omit
`MCP_INTERNAL_ADDR` from that service's environment if it is unnecessary.

## Health checks

Run checks inside the appropriate container, since host ports are not published:

```sh
docker compose exec youtrack-mcp curl -fsS http://127.0.0.1:8080/healthz
docker compose exec youtrack-mcp curl -fsS http://127.0.0.1:8080/readyz
```

Expected responses are `live` and `ready`. Both are static handler responses;
they show that startup completed and the HTTP server responds, not that YouTrack
credentials and connectivity remain valid. Use a read-only MCP call such as
`users` with `{"op":"me"}` to check end-to-end access.

The tunnel has separate operator endpoints on its own loopback port `8080`:

```sh
docker compose exec openai-tunnel curl -fsS http://127.0.0.1:8080/healthz
docker compose exec openai-tunnel curl -fsS http://127.0.0.1:8080/readyz
docker compose exec openai-tunnel curl -fsS http://127.0.0.1:8080/api/status
```

Treat status output as operator information and redact secrets before sharing it.

## Updates and persistence

Update the checked-in source, run the [development checks](development.md), and
rebuild with `docker compose up -d --build`. Changing `TUNNEL_CLIENT_VERSION`
also requires a rebuild. Reconnect clients and refresh discovery after updating.

There are no local database migrations. Business data stays in YouTrack; Compose
has no volume for downloaded attachments. If you need those files to survive
container replacement, mount a writable volume, set `YOUTRACK_DOWNLOAD_DIR` in
the MCP service, and ensure UID `10001` can write to it.

Rotate the direct-client secret by replacing `MCP_AUTH_TOKEN`, recreating the MCP
container, and updating client headers. Rotate the YouTrack or tunnel credentials
in their respective service environment and recreate the affected container.

## Troubleshooting

| Symptom | Check |
|---|---|
| MCP exits before listening | Required environment values, schema fetch/read errors, and the MCP logs. |
| OpenAPI fetch fails | YouTrack base URL, token, network access, and `/api/openapi.json`; use a matching mounted schema if needed. |
| Public endpoint returns `401` | MCP token value and proxy preservation of `Authorization`. |
| A tool reports YouTrack `401`/`403` | Upstream token validity, scopes, and account permissions. |
| Tunnel cannot connect | MCP health, both services' shared network, tunnel credentials, and target `http://youtrack-mcp:8081/mcp`. |
| Proxy returns `404`/`502` | Service name, shared network, port `8080`, and preservation of `/mcp`. |
| Optional settings have no effect | `.env` alone does not forward additional binary settings; add them to Compose's environment mapping. |
| Client has an old tool catalog | Restart after schema changes and refresh client discovery. |
| Attachment upload fails | A compatible authorized file object, an unexpired URL reachable from the container, and an available filename. |
| Download returns an inaccessible path | It is a path inside the MCP server; configure file access or a persistent volume separately. |
| Report looks incomplete | Timezone, holidays, date range, and the 1,000-entry report limit. |

## Working inside sbx

The repository's sandbox has its own `localhost`. Use `host.docker.internal` to
reach host services. Sandbox services must listen on `0.0.0.0` or `::` for host
access, which the user exposes with host-side `sbx ports`. Keep the unauthenticated
listener private when arranging any forwarding.

The documented sandbox HTTPS allowlist does not include all Docker build,
YouTrack, or tunnel destinations. If policy blocks them, update host-side
`sbx policy` for the required destinations or build/deploy outside the sandbox.
Docker access and host credentials are not implied by being inside sbx.
