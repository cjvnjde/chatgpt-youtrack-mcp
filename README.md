# ChatGPT YouTrack MCP on Dokploy

Runs `sensiarion/youtrack-mcp` through OpenAI Secure MCP Tunnel so it can be used from ChatGPT web.

## Dokploy setup

1. Create a **Project** in Dokploy.
2. Inside the project, create a **Compose** service.
3. Select this repository as the source.
4. Use `docker-compose.yml` as the Compose path.
5. Do **not** configure a domain or expose any ports.
6. Add the environment variables below in Dokploy.
7. Deploy the service.

## Required environment variables

```env
CONTROL_PLANE_API_KEY=sk-replace-me
CONTROL_PLANE_TUNNEL_ID=tunnel_replace_me
YOUTRACK_URL=https://my-items.youtrack.cloud
YOUTRACK_TOKEN=perm-replace-me
```

Optional:

```env
TUNNEL_CLIENT_VERSION=v0.0.10
LOG_LEVEL=info
LOG_FORMAT=json
```

Never commit the real `CONTROL_PLANE_API_KEY` or `YOUTRACK_TOKEN`.

## MCP command

The container runs the equivalent of:

```bash
npx -y mcp-bin sensiarion/youtrack-mcp
```

with `YOUTRACK_URL` and `YOUTRACK_TOKEN` passed to the MCP server.

`mcp-bin` requires Node.js 22+ and caches the downloaded YouTrack MCP binary in `/data/mcp-bin`. The Compose service persists that directory in a Docker volume.

## OpenAI tunnel

Create a Secure MCP Tunnel in the OpenAI Platform and place its values in Dokploy:

```env
CONTROL_PLANE_API_KEY=...
CONTROL_PLANE_TUNNEL_ID=tunnel_...
```

The container makes an outbound connection to OpenAI. No inbound port, reverse proxy, HTTPS certificate, or Dokploy domain is required.

## ChatGPT web

After the Dokploy deployment is running:

1. Enable **Developer mode** in ChatGPT.
2. Create a developer-mode app/plugin.
3. Choose **Tunnel** as the connection type.
4. Select the tunnel corresponding to `CONTROL_PLANE_TUNNEL_ID`.
5. Let ChatGPT discover the YouTrack MCP tools.

## Updating the YouTrack MCP

`mcp-bin` caches the resolved binary. To force it to resolve the newest release again, execute inside the running container:

```bash
npx -y mcp-bin expire sensiarion/youtrack-mcp
```

Then restart/redeploy the service.
