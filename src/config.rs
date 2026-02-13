use serde::Deserialize;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(
        "config file not found at .config/dramma.toml !!!! please create it with:\ntoken = \"your-bearer-token\""
    )]
    NotFound,
    #[error("failed to read config file: {0}")]
    ReadError(#[from] std::io::Error),
    #[error("failed to parse config file: {0}")]
    ParseError(#[from] toml::de::Error),
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub token: String,
    pub home_assistant_url: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let config_path = Path::new(".config/dramma.toml");

        if !config_path.exists() {
            return Err(ConfigError::NotFound);
        }

        let content = fs::read_to_string(config_path)?;
        let config: Config = toml::from_str(&content)?;

        Ok(config)
    }
}
