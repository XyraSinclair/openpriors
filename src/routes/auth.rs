use axum::{
    extract::{ConnectInfo, State},
    http::HeaderMap,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{AppState, AuthUser, API_KEY_SCOPES};
use crate::error::ApiError;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/signup", post(signup))
        .route("/auth/login", post(login))
        .route("/auth/api-keys", post(create_api_key))
        .route("/balance", post(get_balance))
        .route("/credits/grant", post(grant_credits))
}

// --- Signup ---

#[derive(Deserialize)]
struct SignupRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct AuthResponse {
    user_id: Uuid,
    token: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

async fn signup(
    State(state): State<Arc<AppState>>,
    client: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SignupRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let client_ip = effective_client_ip(&client, &headers);
    enforce_auth_rate_limit(&state, "signup", client_ip, Some(&body.email))?;

    let email = normalize_email(&body.email)?;

    if !is_valid_email(&email) {
        return Err(ApiError::BadRequest("invalid email".into()));
    }
    if body.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if body.password.len() > 1024 {
        return Err(ApiError::BadRequest("password is too long".into()));
    }

    let password_hash = hash_password(&body.password)?;

    let user_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id",
    )
    .bind(&email)
    .bind(&password_hash)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        if e.to_string().contains("duplicate key") || e.to_string().contains("unique") {
            ApiError::BadRequest("unable to create account".into())
        } else {
            ApiError::Db(e)
        }
    })?;

    let (token, expires_at) = create_session(&state.db, user_id).await?;

    Ok(Json(AuthResponse {
        user_id,
        token,
        expires_at,
    }))
}

// --- Login ---

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

async fn login(
    State(state): State<Arc<AppState>>,
    client: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let client_ip = effective_client_ip(&client, &headers);
    enforce_auth_rate_limit(&state, "login", client_ip, Some(&body.email))?;

    let email = normalize_email(&body.email)?;
    if body.password.len() > 1024 {
        return Err(ApiError::BadRequest("password is too long".into()));
    }

    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, password_hash, account_state FROM users WHERE email = $1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;

    let hash_to_verify = row
        .as_ref()
        .map(|(_, stored_hash, _)| stored_hash.as_str())
        .unwrap_or(dummy_password_hash());

    verify_password(&body.password, hash_to_verify)?;

    let (user_id, _, account_state) =
        row.ok_or_else(|| ApiError::Unauthorized("invalid credentials".into()))?;

    if account_state != "active" {
        return Err(ApiError::Unauthorized("invalid credentials".into()));
    }

    let (token, expires_at) = create_session(&state.db, user_id).await?;

    Ok(Json(AuthResponse {
        user_id,
        token,
        expires_at,
    }))
}

// --- API Keys ---

#[derive(Deserialize)]
struct CreateApiKeyRequest {
    name: Option<String>,
    scopes: Option<Vec<String>>,
}

#[derive(Serialize)]
struct ApiKeyResponse {
    id: Uuid,
    key: String,
    key_prefix: String,
    name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn create_api_key(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<Json<ApiKeyResponse>, ApiError> {
    auth.require_session()?;

    if let Some(name) = body.name.as_deref() {
        if name.len() > 128 {
            return Err(ApiError::BadRequest("API key name is too long".into()));
        }
    }

    let key = generate_api_key();
    let key_prefix = key[..12].to_string();
    let key_hash = blake3::hash(key.as_bytes());
    let scopes = validated_api_key_scopes(body.scopes)?;

    let row = sqlx::query_as::<_, (Uuid, chrono::DateTime<chrono::Utc>)>(
        "INSERT INTO api_keys (user_id, key_hash, key_prefix, name, scopes)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, created_at",
    )
    .bind(auth.user_id)
    .bind(key_hash.as_bytes().as_slice())
    .bind(&key_prefix)
    .bind(&body.name)
    .bind(&scopes)
    .fetch_one(&state.db)
    .await
    .map_err(ApiError::Db)?;

    Ok(Json(ApiKeyResponse {
        id: row.0,
        key,
        key_prefix,
        name: body.name,
        created_at: row.1,
    }))
}

// --- Balance ---

#[derive(Serialize)]
struct BalanceResponse {
    user_id: Uuid,
    balance_nanodollars: i64,
    balance_usd: f64,
}

async fn get_balance(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
) -> Result<Json<BalanceResponse>, ApiError> {
    auth.require_scope("balance:read")?;

    let balance = crate::credits::get_balance(&state.db, auth.user_id).await?;
    Ok(Json(BalanceResponse {
        user_id: auth.user_id,
        balance_nanodollars: balance,
        balance_usd: balance as f64 / 1_000_000_000.0,
    }))
}

// --- Admin Grant ---

#[derive(Deserialize)]
struct GrantRequest {
    user_id: Uuid,
    amount_usd: f64,
    notes: Option<String>,
}

#[derive(Serialize)]
struct GrantResponse {
    user_id: Uuid,
    granted_nanodollars: i64,
    new_balance_nanodollars: i64,
}

async fn grant_credits(
    State(state): State<Arc<AppState>>,
    client: ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<GrantRequest>,
) -> Result<Json<GrantResponse>, ApiError> {
    // Admin auth via header
    let admin_key = state
        .config
        .admin_api_key
        .as_deref()
        .ok_or_else(|| ApiError::Forbidden("admin grants not configured".into()))?;

    let client_ip = effective_client_ip(&client, &headers);

    if !state.config.admin_ip_allowed(client_ip) {
        return Err(ApiError::Forbidden(
            "admin grants not allowed from this IP".into(),
        ));
    }

    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::Unauthorized("missing auth".into()))?;

    if !constant_time_eq(provided.as_bytes(), admin_key.as_bytes()) {
        return Err(ApiError::Forbidden("invalid admin key".into()));
    }

    if !body.amount_usd.is_finite() || body.amount_usd <= 0.0 {
        return Err(ApiError::BadRequest("grant amount must be positive".into()));
    }
    if let Some(notes) = body.notes.as_deref() {
        if notes.len() > 4096 {
            return Err(ApiError::BadRequest("grant notes are too long".into()));
        }
    }

    let scaled = body.amount_usd * 1_000_000_000.0;
    if scaled > i64::MAX as f64 {
        return Err(ApiError::BadRequest("grant amount is too large".into()));
    }

    let nanodollars = scaled.round() as i64;
    let idempotency_key = format!("admin_grant:{}:{}", body.user_id, Uuid::new_v4());

    let new_balance = crate::credits::grant_credits(
        &state.db,
        body.user_id,
        nanodollars,
        &idempotency_key,
        body.notes.as_deref(),
    )
    .await?;

    Ok(Json(GrantResponse {
        user_id: body.user_id,
        granted_nanodollars: nanodollars,
        new_balance_nanodollars: new_balance,
    }))
}

// --- Helpers ---

fn hash_password(password: &str) -> Result<String, ApiError> {
    use argon2::{
        password_hash::{rand_core::OsRng, SaltString},
        Argon2, PasswordHasher,
    };

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(format!("password hash error: {e}")))
}

fn verify_password(password: &str, hash: &str) -> Result<(), ApiError> {
    use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};

    let parsed = PasswordHash::new(hash)
        .map_err(|e| ApiError::Internal(format!("password hash parse error: {e}")))?;

    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| ApiError::Unauthorized("invalid credentials".into()))
}

fn dummy_password_hash() -> &'static str {
    static DUMMY_PASSWORD_HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    DUMMY_PASSWORD_HASH
        .get_or_init(|| {
            hash_password("openpriors::dummy-password")
                .expect("dummy password hash generation should succeed")
        })
        .as_str()
}

async fn create_session(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<(String, chrono::DateTime<chrono::Utc>), ApiError> {
    // Generate token in a block so ThreadRng (which is !Send) is dropped before .await
    let token = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let token_bytes: [u8; 32] = rng.gen();
        format!(
            "ops_{}",
            token_bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    };
    let token_hash = blake3::hash(token.as_bytes());
    let expires_at = chrono::Utc::now() + chrono::Duration::days(30);

    sqlx::query(
        "INSERT INTO user_sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(token_hash.as_bytes().as_slice())
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(ApiError::Db)?;

    Ok((token, expires_at))
}

fn generate_api_key() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 24] = rng.gen();
    format!(
        "opk_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn normalize_email(email: &str) -> Result<String, ApiError> {
    let normalized = email.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest("invalid email".into()));
    }
    if normalized.len() > 320 {
        return Err(ApiError::BadRequest("email is too long".into()));
    }
    Ok(normalized)
}

fn is_valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();

    !local.is_empty() && !domain.is_empty() && parts.next().is_none() && domain.contains('.')
}

fn validated_api_key_scopes(scopes: Option<Vec<String>>) -> Result<Vec<String>, ApiError> {
    let requested = scopes.unwrap_or_else(|| {
        API_KEY_SCOPES
            .iter()
            .map(|scope| (*scope).to_string())
            .collect()
    });

    let mut deduped = BTreeSet::new();
    for scope in requested {
        if !API_KEY_SCOPES.contains(&scope.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "unknown API key scope {scope}"
            )));
        }
        deduped.insert(scope);
    }

    if deduped.is_empty() {
        return Err(ApiError::BadRequest(
            "API keys must include at least one scope".into(),
        ));
    }

    Ok(deduped.into_iter().collect())
}

fn enforce_auth_rate_limit(
    state: &Arc<AppState>,
    route: &str,
    client_ip: IpAddr,
    email: Option<&str>,
) -> Result<(), ApiError> {
    state
        .auth_limiter
        .check(&format!("{route}:ip:{client_ip}"))?;

    if let Some(email) = email {
        let normalized = email.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            let email_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();
            state
                .auth_limiter
                .check(&format!("{route}:email:{email_hash}"))?;
        }
    }

    Ok(())
}

fn effective_client_ip(client: &ConnectInfo<SocketAddr>, headers: &HeaderMap) -> IpAddr {
    let peer_ip = client.0.ip();
    if !peer_ip.is_loopback() {
        return peer_ip;
    }

    forwarded_client_ip(headers).unwrap_or(peer_ip)
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    header_ip(headers, "x-forwarded-for").or_else(|| header_ip(headers, "x-real-ip"))
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();

    for idx in 0..max_len {
        let l = left.get(idx).copied().unwrap_or(0);
        let r = right.get(idx).copied().unwrap_or(0);
        diff |= usize::from(l ^ r);
    }

    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{
        constant_time_eq, dummy_password_hash, effective_client_ip, validated_api_key_scopes,
        verify_password,
    };
    use axum::{extract::ConnectInfo, http::HeaderMap};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn constant_time_eq_handles_length_mismatches() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    #[test]
    fn api_key_scopes_default_to_all_known_scopes() {
        let scopes = validated_api_key_scopes(None).expect("default scopes should validate");
        assert!(scopes.len() >= 3);
        assert!(scopes.iter().any(|scope| scope == "judge:write"));
    }

    #[test]
    fn api_key_scopes_reject_unknown_values() {
        assert!(validated_api_key_scopes(Some(vec!["root".into()])).is_err());
    }

    #[test]
    fn dummy_password_hash_is_valid_for_timing_padding() {
        assert!(verify_password("not-the-password", dummy_password_hash()).is_err());
    }

    #[test]
    fn effective_client_ip_prefers_forwarded_for_on_loopback_proxy() {
        let client = ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 1234)));
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());

        assert_eq!(
            effective_client_ip(&client, &headers),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))
        );
    }

    #[test]
    fn effective_client_ip_ignores_forwarded_for_for_non_loopback_peers() {
        let client = ConnectInfo(SocketAddr::from((Ipv4Addr::new(10, 0, 0, 8), 1234)));
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());

        assert_eq!(
            effective_client_ip(&client, &headers),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8))
        );
    }
}
