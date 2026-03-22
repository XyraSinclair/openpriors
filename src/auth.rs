use axum::{extract::FromRequestParts, http::request::Parts};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::error::ApiError;

pub const API_KEY_SCOPES: &[&str] = &[
    "balance:read",
    "entities:write",
    "attributes:write",
    "judge:write",
    "rate:run",
    "scores:solve",
];

/// Authenticated user extracted from Authorization header.
///
/// Supports two token formats:
/// - `opk_*` prefix → API key (blake3 hash lookup in api_keys)
/// - Otherwise → session token (blake3 hash lookup in user_sessions, check expiry)
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub api_key_id: Option<Uuid>,
    pub role: String,
    pub scopes: Vec<String>,
}

impl AuthUser {
    pub fn require_scope(&self, scope: &str) -> Result<(), ApiError> {
        if self.api_key_id.is_none() {
            return Ok(());
        }

        if self
            .scopes
            .iter()
            .any(|value| value == "*" || value == scope)
        {
            return Ok(());
        }

        Err(ApiError::Forbidden(format!(
            "API key missing scope {scope}"
        )))
    }

    pub fn require_session(&self) -> Result<(), ApiError> {
        if self.api_key_id.is_some() {
            return Err(ApiError::Forbidden("session token required".into()));
        }
        Ok(())
    }

    pub fn require_admin(&self) -> Result<(), ApiError> {
        if self.role == "admin" {
            return Ok(());
        }
        Err(ApiError::Forbidden("admin privileges required".into()))
    }
}

/// AppState shared across all routes.
pub struct AppState {
    pub db: PgPool,
    pub gateway: Arc<dyn cardinal_harness::ChatGateway>,
    pub config: crate::config::Config,
    pub auth_limiter: AuthAttemptLimiter,
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
            let header = auth_header
                .ok_or_else(|| ApiError::Unauthorized("missing Authorization header".into()))?;

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
                let user_state = sqlx::query_as::<_, (String, String)>(
                    "SELECT account_state, role FROM users WHERE id = $1",
                )
                .bind(row.1)
                .fetch_one(&pool)
                .await
                .map_err(ApiError::Db)?;

                if user_state.0 != "active" {
                    return Err(ApiError::Forbidden("account suspended".into()));
                }

                Ok(AuthUser {
                    user_id: row.1,
                    api_key_id: Some(row.0),
                    role: user_state.1,
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

                let user_state = sqlx::query_as::<_, (String, String)>(
                    "SELECT account_state, role FROM users WHERE id = $1",
                )
                .bind(row.0)
                .fetch_one(&pool)
                .await
                .map_err(ApiError::Db)?;

                if user_state.0 != "active" {
                    return Err(ApiError::Forbidden("account suspended".into()));
                }

                Ok(AuthUser {
                    user_id: row.0,
                    api_key_id: None,
                    role: user_state.1,
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

pub struct AuthAttemptLimiter {
    max_attempts: usize,
    window: Duration,
    attempts: Mutex<HashMap<String, Vec<Instant>>>,
}

impl AuthAttemptLimiter {
    pub fn new(max_attempts: usize, window: Duration) -> Self {
        Self {
            max_attempts,
            window,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, key: &str) -> Result<(), ApiError> {
        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);

        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| ApiError::Internal("auth rate limiter poisoned".into()))?;

        attempts.retain(|_, entry| {
            entry.retain(|instant| *instant >= cutoff);
            !entry.is_empty()
        });

        let entry = attempts.entry(key.to_string()).or_default();

        if entry.len() >= self.max_attempts {
            return Err(ApiError::TooManyRequests(
                "too many authentication attempts, try again later".into(),
            ));
        }

        entry.push(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthAttemptLimiter, AuthUser};
    use uuid::Uuid;

    #[test]
    fn api_keys_require_explicit_scope() {
        let user = AuthUser {
            user_id: Uuid::nil(),
            api_key_id: Some(Uuid::nil()),
            role: "user".to_string(),
            scopes: vec!["balance:read".to_string()],
        };

        assert!(user.require_scope("balance:read").is_ok());
        assert!(user.require_scope("judge:write").is_err());
    }

    #[test]
    fn sessions_bypass_scope_checks() {
        let user = AuthUser {
            user_id: Uuid::nil(),
            api_key_id: None,
            role: "user".to_string(),
            scopes: vec![],
        };

        assert!(user.require_scope("judge:write").is_ok());
    }

    #[test]
    fn auth_attempt_limiter_blocks_after_limit() {
        let limiter = AuthAttemptLimiter::new(2, std::time::Duration::from_secs(60));

        assert!(limiter.check("login:ip:127.0.0.1").is_ok());
        assert!(limiter.check("login:ip:127.0.0.1").is_ok());
        assert!(limiter.check("login:ip:127.0.0.1").is_err());
    }

    #[test]
    fn auth_attempt_limiter_expires_old_attempts() {
        let limiter = AuthAttemptLimiter::new(1, std::time::Duration::from_millis(10));

        assert!(limiter.check("login:ip:127.0.0.1").is_ok());
        std::thread::sleep(std::time::Duration::from_millis(25));
        assert!(limiter.check("login:ip:127.0.0.1").is_ok());
    }
}
