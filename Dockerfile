# syntax=docker/dockerfile:1

ARG TUNNEL_CLIENT_VERSION=v0.0.10
ARG YOUTRACK_MCP_VERSION=v0.1.4
ARG RMCP_VERSION=3.0.1

FROM golang:1.26.2-bookworm AS tunnel-builder
ARG TUNNEL_CLIENT_VERSION

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src/tunnel-client
RUN git clone --depth 1 --branch "${TUNNEL_CLIENT_VERSION}" https://github.com/openai/tunnel-client.git .

RUN mkdir -p /out \
    && CGO_ENABLED=0 go build -trimpath -o /out/tunnel-client ./cmd/client

FROM rust:1.88-bookworm AS youtrack-builder
ARG YOUTRACK_MCP_VERSION
ARG RMCP_VERSION

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src/youtrack-mcp
RUN git clone --depth 1 --branch "${YOUTRACK_MCP_VERSION}" https://github.com/sensiarion/youtrack-mcp.git .

# youtrack-mcp v0.1.4 ships with rmcp 1.7, which only supports the legacy
# initialize lifecycle. ChatGPT tunnel discovery uses MCP 2026-07-28 and sends
# server/discover first. rmcp 3.0.1 adds the 2026-07-28 stateless lifecycle.
# RMCP 3 also renamed the MCP content union from Content to ContentBlock.
RUN sed -i "s/rmcp = { version = \"1.7\"/rmcp = { version = \"${RMCP_VERSION}\"/" Cargo.toml \
    && sed -i 's/CallToolResult, Content/CallToolResult, ContentBlock/' src/server.rs \
    && sed -i 's/Content::/ContentBlock::/g' src/server.rs \
    && rm -f Cargo.lock \
    && cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=tunnel-builder /out/tunnel-client /usr/local/bin/tunnel-client
COPY --from=youtrack-builder /src/youtrack-mcp/target/release/youtrack-mcp /usr/local/bin/youtrack-mcp

RUN useradd --create-home --uid 10001 app
USER app

ENTRYPOINT ["/usr/local/bin/tunnel-client", "run"]
