# ChatGPT YouTrack MCP on Dokploy

Runs `sensiarion/youtrack-mcp` through OpenAI Secure MCP Tunnel so it can be used from ChatGPT web.

The `youtrack-mcp/` directory contains the upstream `youtrack-mcp v0.1.4` source with this deployment's compatibility changes applied directly. It uses RMCP 3 for ChatGPT tunnel discovery and accepts ChatGPT file references for attachment uploads.

For unchanged uploads, ChatGPT mounts the conversation file and rewrites the attachment tool's declared `path` file parameter. Authorized temporary file references and base64 remain supported for other MCP clients.

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

Optional tunnel client version pin:

```env
TUNNEL_CLIENT_VERSION=v0.0.10
```

Optional logging:

```env
LOG_LEVEL=info
LOG_FORMAT=json
```

Never commit the real `CONTROL_PLANE_API_KEY` or `YOUTRACK_TOKEN`.

## MCP command

The container runs:

```bash
/usr/local/bin/youtrack-mcp
```

with `YOUTRACK_URL` and `YOUTRACK_TOKEN` passed to the MCP server.

The binary is built directly from the checked-in `youtrack-mcp/` source during the Docker image build.

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
5. Scan/discover the YouTrack MCP tools.

## Health checks

Inside the running Dokploy container:

```bash
curl -fsS http://127.0.0.1:8080/healthz
curl -fsS http://127.0.0.1:8080/readyz
```

Expected responses are `live` and `ready`.

To inspect the tunnel/MCP state:

```bash
curl -fsS http://127.0.0.1:8080/api/status
```

## Updating

Update the checked-in source under `youtrack-mcp/`, run its tests, and redeploy. To update the tunnel client, change `TUNNEL_CLIENT_VERSION` in `docker-compose.yml` / `.env`. The Docker image is rebuilt from source, so there is no persistent `mcp-bin` cache to clear.
