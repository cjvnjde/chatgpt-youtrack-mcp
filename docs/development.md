# Development and architecture

[Documentation home](../README.md#documentation)

The MCP source is checked in under [`youtrack-mcp/`](../youtrack-mcp/).
Root-level Docker and Compose files package it with the tunnel client.

## Build and check

Use Rust through `rustup`. The local toolchain file pins `1.94.0` with rustfmt
and Clippy; the Docker build separately pins Rust `1.88.0`. The manifest's declared
minimum is `1.85`, but its toolchain comments note that dependencies require at
least `1.88`. Use the pinned toolchains rather than relying on the manifest alone.

From the repository root:

```sh
cd youtrack-mcp
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

The resulting binary is `youtrack-mcp/target/release/youtrack-mcp` relative to the
repository root. Tests include configuration, tool schemas, OpenAPI generation,
request handling, HTTP authentication, and attachment behavior; local HTTP
fixtures avoid requiring a live YouTrack instance for those checks.

To verify the container build, run `docker compose build` from the repository
root in an environment with Docker and the necessary network access.

## Run locally

For a process-based MCP client, use the
[stdio configuration](configuration.md#local-stdio-client). The server reserves
stdout for MCP and sends logs to stderr.

For HTTP development, export `YOUTRACK_URL` and `YOUTRACK_TOKEN` securely, then
run from `youtrack-mcp/`:

```sh
export MCP_TRANSPORT=http
export MCP_HTTP_ADDR=0.0.0.0:8080
export MCP_AUTH_TOKEN="$(openssl rand -hex 32)"
unset MCP_INTERNAL_ADDR
cargo run --locked
```

Supply the generated token to your client. This listens on all interfaces for
sandbox/container access; keep development HTTP on a trusted network and use
HTTPS through a reverse proxy for public access. See the
[sandbox networking notes](deployment.md#working-inside-sbx).

## Source map

| File | Responsibility |
|---|---|
| `src/main.rs` | Logging, configuration, startup, and transport selection. |
| `src/config.rs` | YouTrack settings, timezone/calendar parsing, issue expansion, and user aliases. |
| `src/http_transport.rs` | Two HTTP listeners, bearer authentication, health routes, session managers, and shutdown. |
| `src/model.rs` | Curated input types and their JSON schemas. |
| `src/server.rs` | Tool registration, dispatch, catalog assembly, response formatting, and file delivery. |
| `src/youtrack.rs` | REST requests, metadata resolution/caching, issue workflows, and attachment transfer. |
| `src/openapi.rs` | Generated tool names/schemas, parameter/body encoding, and response conversion. |
| `src/report.rs` | Worktime aggregation and expected-day calculations. |
| `src/error.rs` | Application errors and conversion to MCP errors. |
| `../Dockerfile` | Go tunnel build, Rust MCP build, and shared runtime image. |
| `../docker-compose.yml` | Service environment, private networking, and health dependency. |

## Request path

1. The public HTTP listener checks the bearer token before `/mcp` handling;
   the private listener and stdio transport do not apply bearer authentication.
2. RMCP routes the call to a curated handler or a generated API operation.
3. Curated handlers deserialize input and resolve workflow-specific metadata;
   generated handlers build the request from the OpenAPI operation.
4. The YouTrack client sends requests using the configured permanent token.
5. The handler returns JSON text, structured content, or attachment image content,
   depending on the tool. Errors are converted to MCP errors.

Metadata and schemas live in memory. There is no database, job queue, or migration
system. Multi-request mutations have no cross-request transaction. HTTP sessions
are local to the listener's session manager, which matters when adding replicas.

## Changing tools

Update curated input types in `model.rs`, registration and behavior in `server.rs`,
and REST operations in `youtrack.rs`. Keep reporting rules in `report.rs`.
Generated API coverage changes belong in `openapi.rs`; avoid maintaining a manual
list of instance-specific operations.

Verify observable behavior and update the [tool reference](tools.md),
[workflow guide](how-it-works.md), and configuration/deployment docs as needed.
Keep generated output schemas available through `api_schema` without reintroducing
large repeated output models into discovery. Preserve the required file parameter
and `openai/fileParams` metadata when changing ChatGPT attachment uploads.

[`probe-tools.sh`](../probe-tools.sh) is an environment-specific inspection helper:
it expects `/usr/local/bin/youtrack-mcp`, configured credentials, and writes under
`/tmp`. It is not the test suite or a portable client setup command.
