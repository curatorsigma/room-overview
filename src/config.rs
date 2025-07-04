use std::{fs::read_to_string, path::Path};

use axum_server::tls_rustls::RustlsConfig;
use serde::Deserialize;
use sqlx::{Pool, Sqlite, SqlitePool};
use tracing::{event, Level};

use crate::BOOKING_DATABASE_NAME;

#[derive(Debug)]
pub(crate) enum ConfigError {
    Tls(std::io::Error),
    TomlParse(toml::de::Error),
    ConfigFileRead(std::io::Error),
    PoolCreate(sqlx::Error),
}
impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Tls(e) => {
                write!(f, "Unable to create TLS config: {e}")
            }
            Self::TomlParse(e) => {
                write!(f, "Unable to parse config file as toml: {e}")
            }
            Self::ConfigFileRead(e) => {
                write!(f, "Unable to read config file: {e}")
            }
            Self::PoolCreate(e) => {
                write!(f, "Unable to create sqlite pool: {e}")
            }
        }
    }
}
impl core::error::Error for ConfigError {}

#[derive(Debug, Deserialize)]
struct WebConfigData {
    addr: String,
    port: Option<u16>,
    tls_port: Option<u16>,
    tls_cert_file: Option<String>,
    tls_key_file: Option<String>,
}

#[derive(Debug)]
pub(crate) struct WebConfig {
    pub(crate) addr: String,
    pub(crate) port: u16,
    pub(crate) tls_port: u16,
    pub(crate) rustls_config: Option<RustlsConfig>,
}
impl WebConfig {
    async fn try_from_web_config_data(value: WebConfigData) -> Result<Self, ConfigError> {
        let do_tls = value.tls_cert_file.is_some() && value.tls_key_file.is_some();
        let rustls_config = if do_tls {
            match RustlsConfig::from_pem_file(
                value.tls_cert_file.expect("checked is_some"),
                value.tls_key_file.expect("checked is_some"),
            )
            .await
            {
                Ok(x) => Some(x),
                Err(e) => {
                    event!(
                        Level::ERROR,
                        "There was a problem reading the TLS cert/key: {e}"
                    );
                    return Err(ConfigError::Tls(e));
                }
            }
        } else {
            None
        };
        Ok(Self {
            addr: value.addr,
            port: value.port.unwrap_or(80),
            tls_port: value.tls_port.unwrap_or(443),
            rustls_config,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ConfigData {
    pub ct: ChurchToolsConfig,
    pub log_level: String,
    pub rooms: Vec<RoomConfig>,
    pub web: WebConfigData,
}
#[derive(Debug)]
pub(crate) struct Config {
    pub ct: ChurchToolsConfig,
    pub db: Pool<Sqlite>,
    pub log_level: String,
    pub rooms: Vec<RoomConfig>,
    pub web: WebConfig,
}
impl Config {
    async fn try_from_config_data(value: ConfigData) -> Result<Self, ConfigError> {
        let sqlite_connect_options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(BOOKING_DATABASE_NAME)
            .create_if_missing(true);
        let db = SqlitePool::connect_with(sqlite_connect_options)
            .await
            .map_err(ConfigError::PoolCreate)?;

        Ok(Self {
            ct: value.ct,
            db,
            log_level: value.log_level,
            rooms: value.rooms,
            web: WebConfig::try_from_web_config_data(value.web).await?,
        })
    }

    pub async fn create() -> Result<Config, ConfigError> {
        let path = Path::new("/etc/room-overview/config.toml");
        let content = read_to_string(path).map_err(ConfigError::ConfigFileRead)?;
        let config_data: ConfigData = toml::from_str(&content).map_err(ConfigError::TomlParse)?;
        Self::try_from_config_data(config_data).await
    }
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RoomConfig {
    pub churchtools_id: i64,
    pub name: String,
    pub location_hint: String,
}
impl RoomConfig {
    pub(crate) fn ics_location(&self) -> String {
        format!("{} - {}", self.name, self.location_hint)
    }
}

#[derive(Deserialize)]
pub(crate) struct ChurchToolsConfig {
    pub host: String,
    pub login_token: String,
    pub ct_pull_frequency: u64,
}
impl core::fmt::Debug for ChurchToolsConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("ChurchToolsConfig")
            .field("host", &self.host)
            .field("login_token", &"[redacated]")
            .field("ct_pull_frequency", &self.ct_pull_frequency)
            .finish()
    }
}
