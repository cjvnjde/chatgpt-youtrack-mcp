mod config;
mod error;
mod http_transport;
mod model;
mod openapi;
mod report;
mod server;
mod youtrack;

use rmcp::transport::stdio;
use rmcp::ServiceExt;

use crate::config::Config;
use crate::server::Server;
use crate::youtrack::YouTrack;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting youtrack-mcp");

    let cfg = Config::from_env()?;
    let yt = YouTrack::new(cfg)?;
    let server = Server::new(yt).await?;
    let transport = std::env::var("MCP_TRANSPORT").unwrap_or_else(|_| "stdio".into());
    match transport.trim().to_ascii_lowercase().as_str() {
        "stdio" => {
            let service = server.serve(stdio()).await?;
            service.waiting().await?;
        }
        "http" => http_transport::serve(server, http_transport::HttpConfig::from_env()?).await?,
        other => anyhow::bail!("unsupported MCP_TRANSPORT {other:?}; expected stdio or http"),
    }
    Ok(())
}
