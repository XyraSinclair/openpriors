use axum::{
    extract::State,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{AppState, AuthUser};
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
    Json(body): Json<SignupRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    if body.email.is_empty() || !body.email.contains('@') {
        return Err(ApiError::BadRequest("invalid email".into()));
    }
    if body.password.len() < 8 {
        return Err(ApiError::BadRequest("password must be at least 8 characters".into()));
    }

    let password_hash = hash_password(&body.password)?;

    let user_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id",
    )
    .bind(&body.email)
    .bind(&password_hash)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        if e.to_string().contains("duplicate key") || e.to_string().contains("unique") {
            ApiError::BadRequest("email already registered".into())
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
    Json(body): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, password_hash, account_state FROM users WHERE email = $1",
    )
    .bind(&body.email)
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?
    .ok_or_else(|| ApiError::Unauthorized("invalid credentials".into()))?;

    let (user_id, stored_hash, account_state) = row;

    if account_state != "active" {
        return Err(ApiError::Forbidden("account suspended".into()));
    }

    verify_password(&body.password, &stored_hash)?;

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
    let key = generate_api_key();
    let key_prefix = key[..12].to_string();
    let key_hash = blake3::hash(key.as_bytes());
    let scopes = body.scopes.unwrap_or_default();

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
    headers: axum::http::HeaderMap,
    Json(body): Json<GrantRequest>,
) -> Result<Json<GrantResponse>, ApiError> {
    // Admin auth via header
    let admin_key = state.config.admin_api_key.as_deref()
        .ok_or_else(|| ApiError::Forbidden("admin grants not configured".into()))?;

    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::Unauthorized("missing auth".into()))?;

    if provided != admin_key {
        return Err(ApiError::Forbidden("invalid admin key".into()));
    }

    let nanodollars = (body.amount_usd * 1_000_000_000.0) as i64;
    let idempotency_key = format!("admin_grant:{}:{}", body.user_id, chrono::Utc::now().timestamp());

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
    use argon2::{
        password_hash::PasswordHash, Argon2, PasswordVerifier,
    };

    let parsed = PasswordHash::new(hash)
        .map_err(|e| ApiError::Internal(format!("password hash parse error: {e}")))?;

    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| ApiError::Unauthorized("invalid credentials".into()))
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
            token_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
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
