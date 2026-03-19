use std::env;

pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub default_model: String,
    pub anthropic_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub receipt_signing_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            bind_addr: env::var("BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            default_model: env::var("DEFAULT_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-6".to_string()),
            anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty()),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()),
            receipt_signing_key: env::var("RECEIPT_SIGNING_KEY").ok().filter(|s| !s.is_empty()),
        }
    }
}
