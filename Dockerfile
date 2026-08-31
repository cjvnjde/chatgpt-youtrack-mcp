# syntax=docker/dockerfile:1

ARG TUNNEL_CLIENT_VERSION=v0.0.10

FROM golang:1.26.2-bookworm AS tunnel-builder
ARG TUNNEL_CLIENT_VERSION

WORKDIR /src/tunnel-client
RUN git clone --depth 1 --branch "${TUNNEL_CLIENT_VERSION}" https://github.com/openai/tunnel-client.git .

RUN mkdir -p /out \
    && CGO_ENABLED=0 go build -trimpath -o /out/tunnel-client ./cmd/client

FROM rust:1.88-bookworm AS youtrack-builder
ENV RUSTUP_TOOLCHAIN=1.88.0

WORKDIR /src/youtrack-mcp
COPY youtrack-mcp/ .
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=tunnel-builder /out/tunnel-client /usr/local/bin/tunnel-client
COPY --from=youtrack-builder /src/youtrack-mcp/target/release/youtrack-mcp /usr/local/bin/youtrack-mcp

RUN useradd --create-home --uid 10001 app
USER app

ENTRYPOINT ["/usr/local/bin/tunnel-client", "run"]
