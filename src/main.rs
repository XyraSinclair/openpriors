use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::middleware::{self, Next};
use axum::response::Response;
use cardinal_harness::gateway::{NoopUsageSink, ProviderGateway};
use openpriors::auth::{AppState, AuthAttemptLimiter};
use openpriors::config::Config;
use openpriors::db;
use openpriors::routes;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env();
    let bind_addr = config.bind_addr.clone();
    let cors_allowed_origins = config.cors_allowed_origins.clone();
    let pool = db::connect(
        &config.database_url,
        config.database_max_connections,
        config.database_acquire_timeout(),
    )
    .await
    .expect("failed to connect to database");

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        max_connections = config.database_max_connections,
        acquire_timeout_secs = config.database_acquire_timeout_secs,
        "connected to database"
    );

    // Build provider gateway (reads OPENROUTER_API_KEY from env)
    let usage_sink = Arc::new(NoopUsageSink);
    let gateway = ProviderGateway::from_env(usage_sink)
        .expect("failed to create provider gateway — is OPENROUTER_API_KEY set?");

    let auth_limiter = AuthAttemptLimiter::new(
        config.auth_rate_limit_max_attempts,
        std::time::Duration::from_secs(config.auth_rate_limit_window_secs),
    );

    let state = Arc::new(AppState {
        db: pool,
        gateway: Arc::new(gateway),
        config,
        auth_limiter,
    });

    let app = routes::router(state)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(harden_http));

    let app = if cors_allowed_origins.is_empty() {
        app
    } else {
        let parsed: Vec<HeaderValue> = cors_allowed_origins
            .iter()
            .map(|origin| {
                origin
                    .parse()
                    .unwrap_or_else(|_| panic!("invalid CORS_ALLOWED_ORIGINS entry: {origin}"))
            })
            .collect();

        app.layer(
            CorsLayer::new()
                .allow_origin(parsed)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        )
    };

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("failed to bind");

    tracing::info!("listening on {bind_addr}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("server error");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received");
}

async fn harden_http(mut req: Request, next: Next) -> Response {
    let had_authorization = req.headers().contains_key(header::AUTHORIZATION);
    let method = req.method().clone();

    if let Some(value) = req.headers_mut().get_mut(header::AUTHORIZATION) {
        value.set_sensitive(true);
    }

    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "accelerometer=(), camera=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), usb=()",
        ),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data: https:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );

    if had_authorization {
        headers.append(header::VARY, HeaderValue::from_static("authorization"));
    }

    if had_authorization || !matches!(method, Method::GET | Method::HEAD) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }

    response
}
