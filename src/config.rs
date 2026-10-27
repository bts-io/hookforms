//! Settings read from the environment, with the same keys and defaults as the
//! Python version (`selfhosted/api/app/config.py`, pydantic-settings).
//!
//! Like pydantic-settings, keys are matched case-insensitively, real
//! environment variables win over a `.env` file in the working directory, and
//! unknown keys are ignored.

use std::collections::HashMap;

/// Default SQLite file inside the `/data` volume.
pub const DEFAULT_DATABASE_URL: &str = "sqlite:///data/hookforms.db";

#[derive(Debug, Clone)]
pub struct Config {
    /// SQLite URL. A Postgres URL left over from a Python install is ignored
    /// (logged) and the default SQLite file is used instead.
    pub database_url: String,
    pub admin_api_key: String,
    pub app_name: String,
    pub debug: bool,
    /// Comma-separated allowed origins, empty means allow all.
    pub cors_origins: String,
    pub gmail_credentials_path: String,
    pub gmail_token_path: String,
    pub gmail_sender_email: String,
    pub event_retention_days: i64,
    /// Notes about keys that were read but have no effect in this version.
    pub ignored: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    MissingAdminKey,
    Invalid { key: &'static str, value: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::MissingAdminKey => write!(
                f,
                "FATAL: ADMIN_API_KEY environment variable is not set. Refusing to start."
            ),
            ConfigError::Invalid { key, value } => {
                write!(f, "FATAL: invalid value for {key}: {value:?}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Pydantic's lax boolean parsing.
fn parse_bool(key: &'static str, v: &str) -> Result<bool, ConfigError> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "f" | "no" | "n" | "off" => Ok(false),
        _ => Err(ConfigError::Invalid {
            key,
            value: v.to_string(),
        }),
    }
}

impl Config {
    /// Load from the process environment, after merging a `.env` file if one exists.
    pub fn from_env() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        Self::from_map(std::env::vars())
    }

    /// Build from any key/value source. Keys are case-insensitive.
    pub fn from_map<I, K, V>(vars: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        let env: HashMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_ascii_uppercase(), v.into()))
            .collect();
        let get = |k: &str| env.get(k).cloned();
        let mut ignored = Vec::new();

        let database_url = match get("DATABASE_URL").filter(|v| !v.trim().is_empty()) {
            Some(url) if url.starts_with("sqlite:") => url,
            Some(url) if url.starts_with("postgres") => {
                ignored.push(
                    "DATABASE_URL points at Postgres; this version uses SQLite, \
                     using the default file (import old data with the import command)"
                        .to_string(),
                );
                DEFAULT_DATABASE_URL.to_string()
            }
            Some(other) => {
                return Err(ConfigError::Invalid {
                    key: "DATABASE_URL",
                    value: other,
                });
            }
            None => DEFAULT_DATABASE_URL.to_string(),
        };

        for key in ["REDIS_URL", "REDIS_PASSWORD", "POSTGRES_PASSWORD"] {
            if get(key).is_some_and(|v| !v.is_empty()) {
                ignored.push(format!("{key} is set but not used (no Postgres or Redis)"));
            }
        }

        let admin_api_key = get("ADMIN_API_KEY").unwrap_or_default();
        if admin_api_key.is_empty() {
            return Err(ConfigError::MissingAdminKey);
        }

        let debug = match get("DEBUG") {
            Some(v) => parse_bool("DEBUG", &v)?,
            None => false,
        };

        let event_retention_days = match get("EVENT_RETENTION_DAYS") {
            Some(v) => v.trim().parse::<i64>().map_err(|_| ConfigError::Invalid {
                key: "EVENT_RETENTION_DAYS",
                value: v,
            })?,
            None => 30,
        };

        Ok(Config {
            database_url,
            admin_api_key,
            app_name: get("APP_NAME").unwrap_or_else(|| "HookForms".into()),
            debug,
            cors_origins: get("CORS_ORIGINS").unwrap_or_default(),
            gmail_credentials_path: get("GMAIL_CREDENTIALS_PATH")
                .unwrap_or_else(|| "/app/config/gmail/credentials.json".into()),
            gmail_token_path: get("GMAIL_TOKEN_PATH")
                .unwrap_or_else(|| "/app/config/gmail/token.json".into()),
            gmail_sender_email: get("GMAIL_SENDER_EMAIL").unwrap_or_default(),
            event_retention_days,
            ignored,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        Config::from_map(pairs.iter().map(|(k, v)| (*k, v.to_string())))
    }

    #[test]
    fn defaults_match_python() {
        let c = cfg(&[("ADMIN_API_KEY", "secret")]).unwrap();
        assert_eq!(c.database_url, DEFAULT_DATABASE_URL);
        assert_eq!(c.app_name, "HookForms");
        assert!(!c.debug);
        assert_eq!(c.event_retention_days, 30);
        assert_eq!(c.gmail_token_path, "/app/config/gmail/token.json");
        assert_eq!(
            c.gmail_credentials_path,
            "/app/config/gmail/credentials.json"
        );
        assert_eq!(c.cors_origins, "");
        assert!(c.ignored.is_empty());
    }

    #[test]
    fn missing_admin_key_is_fatal() {
        assert_eq!(cfg(&[]).unwrap_err(), ConfigError::MissingAdminKey);
        assert_eq!(
            cfg(&[("ADMIN_API_KEY", "")]).unwrap_err(),
            ConfigError::MissingAdminKey
        );
    }

    #[test]
    fn keys_are_case_insensitive_and_parsed() {
        let c = cfg(&[
            ("admin_api_key", "k"),
            ("Debug", "yes"),
            ("EVENT_RETENTION_DAYS", "7"),
            ("CORS_ORIGINS", " https://a.example , ,https://b.example"),
            ("DATABASE_URL", "sqlite::memory:"),
        ])
        .unwrap();
        assert!(c.debug);
        assert_eq!(c.event_retention_days, 7);
        assert_eq!(c.cors_origins, " https://a.example , ,https://b.example");
        assert_eq!(c.database_url, "sqlite::memory:");
    }

    #[test]
    fn postgres_and_redis_keys_are_ignored_with_a_note() {
        let c = cfg(&[
            ("ADMIN_API_KEY", "k"),
            (
                "DATABASE_URL",
                "postgresql+asyncpg://u:p@postgres:5432/hookforms",
            ),
            ("REDIS_URL", "redis://:p@redis:6379"),
            ("POSTGRES_PASSWORD", "p"),
        ])
        .unwrap();
        assert_eq!(c.database_url, DEFAULT_DATABASE_URL);
        assert_eq!(c.ignored.len(), 3);
    }

    #[test]
    fn bad_values_are_rejected() {
        assert!(matches!(
            cfg(&[("ADMIN_API_KEY", "k"), ("EVENT_RETENTION_DAYS", "soon")]),
            Err(ConfigError::Invalid {
                key: "EVENT_RETENTION_DAYS",
                ..
            })
        ));
        assert!(matches!(
            cfg(&[("ADMIN_API_KEY", "k"), ("DEBUG", "maybe")]),
            Err(ConfigError::Invalid { key: "DEBUG", .. })
        ));
    }
}
