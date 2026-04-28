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

/// A single playable game entry, configured via `dramma.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameEntry {
    pub name: String,
    pub core: String,
    pub rom: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub token: Option<String>,
    pub home_assistant_url: String,
    pub hass_api_port: u16,
    pub cashcode_serial_port: String,
    pub cctalk_serial_port: String,
    pub cctalk_coin_overrides: Vec<[i32; 2]>,
    pub stats_db_path: String,
    pub retroarch_command: String,
    pub games: Vec<GameEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            token: None,
            home_assistant_url: "https://ha.hackem.cc/web-dramma/0?BrowserID=dramma".to_string(),
            hass_api_port: 8321,
            cashcode_serial_port:
                "/dev/serial/by-id/usb-Prolific_Technology_Inc._USB-Serial_Controller_D-if00-port0"
                    .to_string(),
            cctalk_serial_port: "/dev/ttyUSB0".to_string(),
            cctalk_coin_overrides: Vec::new(),
            stats_db_path: "data/Stats.db".to_string(),
            retroarch_command: "retroarch".to_string(),
            games: Vec::new(),
        }
    }
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
