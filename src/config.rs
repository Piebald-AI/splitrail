use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub upload: UploadConfig,
    pub formatting: FormattingConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServerConfig {
    pub url: String,
    pub api_token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadConfig {
    pub auto_upload: bool,
    pub upload_today_only: bool,
    pub retry_attempts: u32,
    pub last_date_uploaded: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FormattingConfig {
    pub number_comma: bool,
    pub number_human: bool,
    pub locale: String,
    pub decimal_places: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                url: "https://splitrail.dev".to_string(),
                api_token: "".to_string(),
            },
            upload: UploadConfig {
                auto_upload: false,
                upload_today_only: false,
                retry_attempts: 3,
                last_date_uploaded: 0,
            },
            formatting: FormattingConfig {
                number_comma: false,
                number_human: false,
                locale: "en".to_string(),
                decimal_places: 2,
            },
        }
    }
}

thread_local! {
    static TEST_CONFIG_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_config_path(path: PathBuf) {
    TEST_CONFIG_PATH.with(|p| *p.borrow_mut() = Some(path));
}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        #[cfg(test)]
        {
            if let Some(path) = TEST_CONFIG_PATH.with(|p| p.borrow().clone()) {
                return Ok(path);
            }
        }

        Ok(dirs::home_dir()
            .context("Could not find home directory")?
            .join(".splitrail.toml"))
    }

    pub fn load() -> Result<Option<Config>> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&config_path).context("Failed to read config file")?;

        let config: Config = toml::from_str(&content).context("Failed to parse config file")?;

        Ok(Some(config))
    }

    pub fn save(&self, silent: bool) -> Result<()> {
        let config_path = Self::config_path()?;
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&config_path, content).context("Failed to write config file")?;

        if !silent {
            println!("✅ Configuration saved to: {}", config_path.display());
        }

        Ok(())
    }

    pub fn set_api_token(&mut self, token: String) {
        self.server.api_token = token;
    }

    pub fn set_auto_upload(&mut self, enabled: bool) {
        self.upload.auto_upload = enabled;
    }

    pub fn set_upload_today_only(&mut self, enabled: bool) {
        self.upload.upload_today_only = enabled;
    }

    pub fn is_configured(&self) -> bool {
        !self.server.api_token.is_empty() && !self.server.url.is_empty()
    }

    pub fn is_api_token_missing(&self) -> bool {
        self.server.api_token.is_empty()
    }

    pub fn is_server_url_missing(&self) -> bool {
        self.server.url.is_empty()
    }

    pub fn set_last_date_uploaded(&mut self, date: i64) {
        self.upload.last_date_uploaded = date;
    }
}

// CLI helper functions
pub fn create_default_config(overwrite: bool) -> Result<()> {
    let config = Config::default();
    if !std::fs::exists(Config::config_path()?)? || overwrite {
        config.save(true)?;

        println!("📝 Created default configuration file.");
        println!("📍 Edit it with your Splitrail Cloud API token:");
        println!("   splitrail config set api-token ...");
        println!("or");
        println!("   {}", Config::config_path()?.display());
    } else {
        println!("Configuration already exists.  Pass `--overwrite` to overwrite.");
    }

    Ok(())
}

pub fn show_config() -> Result<()> {
    match Config::load()? {
        Some(config) => {
            println!("🔧 Current configuration:");
            println!(
                "   API Token: {}",
                if config.server.api_token.is_empty() {
                    "Not set"
                } else {
                    "Set"
                }
            );
            println!("   Auto Upload: {}", config.upload.auto_upload);
            println!("   Upload Today Only: {}", config.upload.upload_today_only);
            println!("   Number Comma: {}", config.formatting.number_comma);
            println!("   Number Human: {}", config.formatting.number_human);
            println!("   Locale: {}", config.formatting.locale);
            println!("   Decimal Places: {}", config.formatting.decimal_places);
        }
        None => {
            println!("❌ No configuration file found.");
            println!("   Run 'splitrail config init' to create one.");
        }
    }
    Ok(())
}

pub fn set_config_value(key: &str, value: &str) -> Result<()> {
    let mut config = Config::load()?.unwrap_or_default();

    match key {
        "api-token" => config.set_api_token(value.to_string()),
        "auto-upload" => {
            let enabled = value
                .parse::<bool>()
                .context("Invalid boolean value. Use 'true' or 'false'")?;
            config.set_auto_upload(enabled);
        }
        "upload-today-only" => {
            let enabled = value
                .parse::<bool>()
                .context("Invalid boolean value. Use 'true' or 'false'")?;
            config.set_upload_today_only(enabled);
        }
        "number-comma" => {
            let enabled = value
                .parse::<bool>()
                .context("Invalid boolean value. Use 'true' or 'false'")?;
            config.formatting.number_comma = enabled;
        }
        "number-human" => {
            let enabled = value
                .parse::<bool>()
                .context("Invalid boolean value. Use 'true' or 'false'")?;
            config.formatting.number_human = enabled;
        }
        "locale" => {
            config.formatting.locale = value.to_string();
        }
        "decimal-places" => {
            let places = value.parse::<usize>().context("Invalid number value")?;
            config.formatting.decimal_places = places;
        }
        _ => anyhow::bail!("Unknown config key: {}", key),
    }

    config.save(false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_config() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let config_path = dir.path().join(".splitrail.toml");
        set_test_config_path(config_path.clone());
        (dir, config_path)
    }

    #[test]
    fn default_config_round_trip() {
        let (_dir, _path) = setup_test_config();
        // Ensure there is a default config on disk using the CLI helper.
        create_default_config(true).expect("create_default_config");

        let loaded = Config::load()
            .expect("load config")
            .expect("config should exist");

        assert_eq!(loaded.server.url, "https://splitrail.dev");
        assert_eq!(loaded.server.api_token, "");
        assert!(!loaded.upload.auto_upload);
        assert_eq!(loaded.formatting.locale, "en");
    }

    #[test]
    fn set_config_value_behaviour() {
        let (_dir, _path) = setup_test_config();

        // Ensure base config exists.
        create_default_config(true).expect("create_default_config");

        set_config_value("api-token", "TEST_TOKEN").expect("set api-token");
        set_config_value("auto-upload", "true").expect("set auto-upload");
        set_config_value("upload-today-only", "true").expect("set upload-today-only");
        set_config_value("number-comma", "true").expect("set number-comma");
        set_config_value("number-human", "true").expect("set number-human");
        set_config_value("locale", "de").expect("set locale");
        set_config_value("decimal-places", "3").expect("set decimal-places");

        let cfg = Config::load()
            .expect("load config")
            .expect("config should exist");

        assert_eq!(cfg.server.api_token, "TEST_TOKEN");
        assert!(cfg.upload.auto_upload);
        assert!(cfg.upload.upload_today_only);
        assert!(cfg.formatting.number_comma);
        assert!(cfg.formatting.number_human);
        assert_eq!(cfg.formatting.locale, "de");
        assert_eq!(cfg.formatting.decimal_places, 3);

        let err = set_config_value("unknown-key", "value").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Unknown config key"),
            "unexpected error message: {msg}"
        );
        let err = set_config_value("auto-upload", "not-a-bool").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Invalid boolean value"),
            "unexpected error message: {msg}"
        );
    }
}
