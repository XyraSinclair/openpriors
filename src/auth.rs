use axum::{
    extract::FromRequestParts,
    http::request::Parts,
};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::error::ApiError;

/// Authenticated user extracted from Authorization header.
///
/// Supports two token formats:
/// - `opk_*` prefix → API key (blake3 hash lookup in api_keys)
/// - Otherwise → session token (blake3 hash lookup in user_sessions, check expiry)
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub api_key_id: Option<Uuid>,
    pub scopes: Vec<String>,
}

/// AppState shared across all routes.
pub struct AppState {
    pub db: PgPool,
    pub gateway: Arc<dyn cardinal_harness::ChatGateway>,
    pub config: crate::config::Config,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let pool = state.db.clone();
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        async move {
            let header = auth_header.ok_or_else(|| {
                ApiError::Unauthorized("missing Authorization header".into())
            })?;

            let token = header
                .strip_prefix("Bearer ")
                .ok_or_else(|| ApiError::Unauthorized("expected Bearer token".into()))?;

            if token.starts_with("opk_") {
                // API key auth
                let key_hash = blake3::hash(token.as_bytes());
                let row = sqlx::query_as::<_, (Uuid, Uuid, Vec<String>)>(
                    "SELECT id, user_id, scopes FROM api_keys
                     WHERE key_hash = $1 AND revoked_at IS NULL",
                )
                .bind(key_hash.as_bytes().as_slice())
                .fetch_optional(&pool)
                .await
                .map_err(ApiError::Db)?
                .ok_or_else(|| ApiError::Unauthorized("invalid API key".into()))?;

                // Check user is active
                let account_state = sqlx::query_scalar::<_, String>(
                    "SELECT account_state FROM users WHERE id = $1",
                )
                .bind(row.1)
                .fetch_one(&pool)
                .await
                .map_err(ApiError::Db)?;

                if account_state != "active" {
                    return Err(ApiError::Forbidden("account suspended".into()));
                }

                Ok(AuthUser {
                    user_id: row.1,
                    api_key_id: Some(row.0),
                    scopes: row.2,
                })
            } else {
                // Session token auth
                let token_hash = blake3::hash(token.as_bytes());
                let row = sqlx::query_as::<_, (Uuid,)>(
                    "SELECT user_id FROM user_sessions
                     WHERE token_hash = $1 AND expires_at > now()",
                )
                .bind(token_hash.as_bytes().as_slice())
                .fetch_optional(&pool)
                .await
                .map_err(ApiError::Db)?
                .ok_or_else(|| ApiError::Unauthorized("invalid or expired session".into()))?;

                let account_state = sqlx::query_scalar::<_, String>(
                    "SELECT account_state FROM users WHERE id = $1",
                )
                .bind(row.0)
                .fetch_one(&pool)
                .await
                .map_err(ApiError::Db)?;

                if account_state != "active" {
                    return Err(ApiError::Forbidden("account suspended".into()));
                }

                Ok(AuthUser {
                    user_id: row.0,
                    api_key_id: None,
                    scopes: vec![],
                })
            }
        }
    }
}

/// Optional auth — doesn't reject unauthenticated requests.
#[derive(Debug, Clone)]
pub struct MaybeAuth(pub Option<AuthUser>);

impl FromRequestParts<Arc<AppState>> for MaybeAuth {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        async move {
            match AuthUser::from_request_parts(parts, state).await {
                Ok(user) => Ok(MaybeAuth(Some(user))),
                Err(_) => Ok(MaybeAuth(None)),
            }
        }
    }
}
