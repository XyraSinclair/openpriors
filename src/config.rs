use std::env;

pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub default_model: String,
    pub openrouter_api_key: String,
    pub admin_api_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            bind_addr: env::var("BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            default_model: env::var("DEFAULT_MODEL")
                .unwrap_or_else(|_| "openai/gpt-5-mini".to_string()),
            openrouter_api_key: env::var("OPENROUTER_API_KEY")
                .expect("OPENROUTER_API_KEY must be set"),
            admin_api_key: env::var("ADMIN_API_KEY").ok().filter(|s| !s.is_empty()),
        }
    }
}
