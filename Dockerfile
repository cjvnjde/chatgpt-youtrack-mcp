# syntax=docker/dockerfile:1

ARG TUNNEL_CLIENT_VERSION=v0.0.10

FROM golang:1.26.2-bookworm AS tunnel-builder
ARG TUNNEL_CLIENT_VERSION

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
RUN git clone --depth 1 --branch "${TUNNEL_CLIENT_VERSION}" https://github.com/openai/tunnel-client.git .

RUN mkdir -p /out \
    && CGO_ENABLED=0 go build -trimpath -o /out/tunnel-client ./cmd/client

FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tar unzip xz-utils \
    && rm -rf /var/lib/apt/lists/*

COPY --from=tunnel-builder /out/tunnel-client /usr/local/bin/tunnel-client

RUN mkdir -p /data/mcp-bin \
    && chown -R node:node /data/mcp-bin

ENV MCP_BIN_DIR=/data/mcp-bin

USER node

ENTRYPOINT ["/usr/local/bin/tunnel-client", "run"]
