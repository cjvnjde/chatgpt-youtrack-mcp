use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

use crate::server::Server;

const DEFAULT_HTTP_ADDRESS: &str = "0.0.0.0:8080";
const MIN_TOKEN_BYTES: usize = 32;

#[derive(Clone)]
struct BearerToken(Arc<[u8]>);

impl BearerToken {
    fn new(token: String) -> anyhow::Result<Self> {
        if token.len() < MIN_TOKEN_BYTES {
            bail!("MCP_AUTH_TOKEN must contain at least {MIN_TOKEN_BYTES} bytes");
        }
        if token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            bail!("MCP_AUTH_TOKEN must not contain whitespace");
        }
        Ok(Self(Arc::from(token.into_bytes())))
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        let Some((scheme, credential)) = value.split_once(' ') else {
            return false;
        };
        scheme.eq_ignore_ascii_case("Bearer")
            && bool::from(credential.as_bytes().ct_eq(self.0.as_ref()))
    }
}

pub(crate) struct HttpConfig {
    address: SocketAddr,
    internal_address: Option<SocketAddr>,
    token: BearerToken,
}

impl HttpConfig {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let address = std::env::var("MCP_HTTP_ADDR")
            .unwrap_or_else(|_| DEFAULT_HTTP_ADDRESS.to_string())
            .parse()
            .context("MCP_HTTP_ADDR must be an IP socket address such as 0.0.0.0:8080")?;
        let internal_address = std::env::var("MCP_INTERNAL_ADDR")
            .ok()
            .filter(|address| !address.trim().is_empty())
            .map(|address| {
                address
                    .parse()
                    .context("MCP_INTERNAL_ADDR must be an IP socket address such as 0.0.0.0:8081")
            })
            .transpose()?;
        if internal_address == Some(address) {
            bail!("MCP_INTERNAL_ADDR must differ from MCP_HTTP_ADDR");
        }
        let token = std::env::var("MCP_AUTH_TOKEN")
            .context("MCP_AUTH_TOKEN is required when MCP_TRANSPORT=http")?;
        Ok(Self {
            address,
            internal_address,
            token: BearerToken::new(token)?,
        })
    }
}

async fn require_bearer(token: BearerToken, request: Request, next: Next) -> Response {
    if token.authorizes(request.headers()) {
        return next.run(request).await;
    }

    let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    response
        .headers_mut()
        .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn with_auth(router: Router, token: Option<BearerToken>) -> Router {
    let Some(token) = token else {
        return router;
    };
    router.layer(middleware::from_fn(
        move |request: Request<Body>, next: Next| {
            let token = token.clone();
            async move { require_bearer(token, request, next).await }
        },
    ))
}

fn mcp_app(
    server: Server,
    cancellation: CancellationToken,
    token: Option<BearerToken>,
    health_routes: bool,
) -> Router {
    let factory_server = server.clone();
    let service = StreamableHttpService::new(
        move || Ok(factory_server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(cancellation),
    );
    let mcp = with_auth(Router::new().nest_service("/mcp", service), token);
    if health_routes {
        Router::new()
            .merge(mcp)
            .route("/healthz", get(|| async { "live" }))
            .route("/readyz", get(|| async { "ready" }))
    } else {
        mcp
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(%error, "failed to listen for shutdown signal");
    }
}

pub(crate) async fn serve(server: Server, config: HttpConfig) -> anyhow::Result<()> {
    let HttpConfig {
        address,
        internal_address,
        token,
    } = config;
    let cancellation = CancellationToken::new();
    let public_app = mcp_app(
        server.clone(),
        cancellation.child_token(),
        Some(token),
        true,
    );
    let public_listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("bind authenticated MCP HTTP listener at {address}"))?;
    tracing::info!(%address, endpoint = "/mcp", "serving authenticated public MCP over HTTP");

    if let Some(internal_address) = internal_address {
        let internal_app = mcp_app(server, cancellation.child_token(), None, false);
        let internal_listener = tokio::net::TcpListener::bind(internal_address)
            .await
            .with_context(|| {
                format!("bind unauthenticated internal MCP HTTP listener at {internal_address}")
            })?;
        tracing::info!(
            address = %internal_address,
            endpoint = "/mcp",
            "serving unauthenticated internal MCP for the tunnel"
        );

        let public_shutdown = cancellation.child_token();
        let internal_shutdown = cancellation.child_token();
        let public = axum::serve(public_listener, public_app)
            .with_graceful_shutdown(public_shutdown.cancelled_owned())
            .into_future();
        let internal = axum::serve(internal_listener, internal_app)
            .with_graceful_shutdown(internal_shutdown.cancelled_owned())
            .into_future();
        tokio::pin!(public);
        tokio::pin!(internal);
        tokio::select! {
            result = &mut public => {
                cancellation.cancel();
                result.context("serve authenticated MCP HTTP endpoint")?;
                internal.await.context("stop internal MCP HTTP endpoint")?;
            }
            result = &mut internal => {
                cancellation.cancel();
                result.context("serve internal MCP HTTP endpoint")?;
                public.await.context("stop authenticated MCP HTTP endpoint")?;
            }
            _ = shutdown_signal() => {
                cancellation.cancel();
                let (public_result, internal_result) = tokio::join!(public, internal);
                public_result.context("stop authenticated MCP HTTP endpoint")?;
                internal_result.context("stop internal MCP HTTP endpoint")?;
            }
        }
    } else {
        let public_shutdown = cancellation.child_token();
        let public = axum::serve(public_listener, public_app)
            .with_graceful_shutdown(public_shutdown.cancelled_owned())
            .into_future();
        tokio::pin!(public);
        tokio::select! {
            result = &mut public => {
                result.context("serve authenticated MCP HTTP endpoint")?;
            }
            _ = shutdown_signal() => {
                cancellation.cancel();
                public.await.context("stop authenticated MCP HTTP endpoint")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn headers(value: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = value {
            headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn bearer_auth_accepts_only_the_exact_token() {
        let token = BearerToken::new(TOKEN.into()).unwrap();

        assert!(token.authorizes(&headers(Some(&format!("Bearer {TOKEN}")))));
        assert!(token.authorizes(&headers(Some(&format!("bearer {TOKEN}")))));
        assert!(!token.authorizes(&headers(None)));
        assert!(!token.authorizes(&headers(Some("Basic abc"))));
        assert!(!token.authorizes(&headers(Some("Bearer wrong"))));
        assert!(!token.authorizes(&headers(Some(&format!("Bearer {TOKEN} extra")))));
    }

    #[test]
    fn bearer_auth_rejects_weak_or_malformed_secrets() {
        assert!(BearerToken::new("short".into()).is_err());
        assert!(BearerToken::new(format!("{TOKEN} suffix")).is_err());
        assert!(BearerToken::new(TOKEN.into()).is_ok());
    }

    #[tokio::test]
    async fn middleware_rejects_missing_and_wrong_credentials() {
        for authorization in [None, Some("Bearer wrong"), Some("Basic wrong")] {
            let app = with_auth(
                Router::new().route("/mcp", get(|| async { "ok" })),
                Some(BearerToken::new(TOKEN.into()).unwrap()),
            );
            let mut request = Request::builder().uri("/mcp");
            if let Some(authorization) = authorization {
                request = request.header(AUTHORIZATION, authorization);
            }
            let response = app
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(WWW_AUTHENTICATE),
                Some(&HeaderValue::from_static("Bearer"))
            );
        }
    }

    #[tokio::test]
    async fn middleware_allows_the_exact_bearer_credential() {
        let app = with_auth(
            Router::new().route("/mcp", get(|| async { "ok" })),
            Some(BearerToken::new(TOKEN.into()).unwrap()),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/mcp")
                    .header(AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn internal_listener_does_not_require_credentials() {
        let app = with_auth(Router::new().route("/mcp", get(|| async { "ok" })), None);
        let response = app
            .oneshot(Request::builder().uri("/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
