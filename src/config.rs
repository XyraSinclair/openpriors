use std::env;
use std::net::IpAddr;
use std::time::Duration;

pub struct Config {
    pub database_url: String,
    pub database_max_connections: u32,
    pub database_acquire_timeout_secs: u64,
    pub bind_addr: String,
    pub default_model: String,
    pub admin_api_key: Option<String>,
    pub admin_allowed_ips: Vec<IpAddr>,
    pub cors_allowed_origins: Vec<String>,
    pub public_judgements: bool,
    pub auth_rate_limit_max_attempts: usize,
    pub auth_rate_limit_window_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            database_max_connections: parse_u32("DATABASE_MAX_CONNECTIONS", 20),
            database_acquire_timeout_secs: parse_u64("DATABASE_ACQUIRE_TIMEOUT_SECS", 10),
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            default_model: env::var("DEFAULT_MODEL")
                .unwrap_or_else(|_| "openai/gpt-5-mini".to_string()),
            admin_api_key: env::var("ADMIN_API_KEY").ok().filter(|s| !s.is_empty()),
            admin_allowed_ips: parse_ip_list("ADMIN_ALLOWED_IPS"),
            cors_allowed_origins: parse_csv("CORS_ALLOWED_ORIGINS"),
            public_judgements: parse_bool("PUBLIC_JUDGEMENTS", false),
            auth_rate_limit_max_attempts: parse_usize("AUTH_RATE_LIMIT_MAX_ATTEMPTS", 10),
            auth_rate_limit_window_secs: parse_u64("AUTH_RATE_LIMIT_WINDOW_SECS", 300),
        }
    }

    pub fn admin_ip_allowed(&self, ip: IpAddr) -> bool {
        if self.admin_allowed_ips.is_empty() {
            return ip.is_loopback();
        }
        self.admin_allowed_ips.contains(&ip)
    }

    pub fn database_acquire_timeout(&self) -> Duration {
        Duration::from_secs(self.database_acquire_timeout_secs)
    }
}

fn parse_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn parse_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_csv(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_ip_list(name: &str) -> Vec<IpAddr> {
    parse_csv(name)
        .into_iter()
        .filter_map(|value| value.parse().ok())
        .collect()
}
