# YouTrack MCP for ChatGPT and remote agents

A self-hosted Rust MCP server for managing YouTrack issues, comments, articles,
time tracking, and attachments from ChatGPT or other MCP clients.

It provides 17 curated workflow tools, a typed `api_*` tool for every operation
advertised by your YouTrack instance, and `api_schema` for output-schema lookup.
Both remote connection paths use the same server and YouTrack credentials:

- ChatGPT connects through the OpenAI Secure MCP Tunnel on private port `8081`.
- Direct clients connect through an HTTPS reverse proxy to bearer-protected port `8080`.

## Documentation

- [How it works and common workflows](docs/how-it-works.md)
- [MCP tool reference](docs/tools.md)
- [Configuration and client connections](docs/configuration.md)
- [Deployment, health checks, and troubleshooting](docs/deployment.md)
- [Development and architecture](docs/development.md)
- [Suggested assistant prompts](docs/prompts.md)

## Quick start

```sh
cp .env.example .env
# Fill in YouTrack credentials, tunnel credentials, and a random MCP_AUTH_TOKEN.
# Generate the bearer token with: openssl rand -hex 32
docker compose up -d --build
```

Follow the [deployment guide](docs/deployment.md) to connect the tunnel and
configure HTTPS for direct clients. Compose publishes no host ports; keep
unauthenticated port `8081` private. All callers act with the permissions of
`YOUTRACK_TOKEN`.
