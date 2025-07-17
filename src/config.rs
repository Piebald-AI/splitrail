use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub upload: UploadConfig,
    pub formatting: FormattingConfig,
    pub last_date_uploaded: i64,
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
    pub timeout_seconds: u64,
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
                timeout_seconds: 30,
            },
            formatting: FormattingConfig {
                number_comma: false,
                number_human: false,
                locale: "en".to_string(),
                decimal_places: 2,
            },
            last_date_uploaded: 0,  
        }
    }
}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        Ok(home::home_dir()
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

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&config_path, content).context("Failed to write config file")?;

        println!("✅ Configuration saved to: {}", config_path.display());
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

    pub fn set_last_date_uploaded(&mut self, date: i64) {
        self.last_date_uploaded = date;
    }
}

// CLI helper functions
pub fn create_default_config() -> Result<()> {
    let config = Config::default();
    config.save()?;

    println!("📝 Created default configuration file.");
    println!("📍 Edit it with your server URL and API token:");
    println!("   {}", Config::config_path()?.display());

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
            let places = value
                .parse::<usize>()
                .context("Invalid number value")?;
            config.formatting.decimal_places = places;
        }
        _ => anyhow::bail!("Unknown config key: {}", key),
    }

    config.save()?;
    println!("✅ Updated {} to: {}", key, value);
    Ok(())
}
