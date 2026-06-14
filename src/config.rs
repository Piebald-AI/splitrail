use crate::models::ModelInfo;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub upload: UploadConfig,
    pub formatting: FormattingConfig,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub models: HashMap<String, ModelInfo>,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
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
}

/// Runtime upload progress state, persisted separately from user configuration.
///
/// Stored in the platform state directory (e.g. `~/.local/state/splitrail/state.toml`
/// on Linux) so that incremental upload checkpoints do not pollute the
/// user-editable config file.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct UploadState {
    /// Timestamp (milliseconds since Unix epoch) of the last successfully uploaded message.
    /// Used to filter out already-uploaded messages on the next run.
    pub last_date_uploaded: i64,
}

fn default_currency_symbol() -> String {
    "$".to_string()
}

fn default_view() -> String {
    "daily".to_string()
}

fn default_cost_decimal_places() -> usize {
    2
}

fn default_accent_color() -> String {
    "cyan".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FormattingConfig {
    pub number_comma: bool,
    pub number_human: bool,
    pub locale: String,
    pub decimal_places: usize,
    /// Symbol shown before cost amounts (e.g. "$", "€", "£"). Default "$".
    #[serde(default = "default_currency_symbol")]
    pub currency_symbol: String,
    /// Decimal places used for cost amounts (e.g. 2 -> $1.23, 0 -> $1). Default 2.
    #[serde(default = "default_cost_decimal_places")]
    pub cost_decimal_places: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TuiConfig {
    #[serde(default)]
    pub reverse_sort_default: bool,
    #[serde(default)]
    pub hide_empty_periods: bool,
    /// Aggregation the TUI opens in: "daily" | "weekly" | "monthly" | "yearly".
    #[serde(default = "default_view")]
    pub default_view: String,
    /// Tab the TUI opens on, by tool name (e.g. "Claude Code"). Empty / "All
    /// Tools" opens the combined first tab.
    #[serde(default)]
    pub default_tab: String,
    /// Require a second 'q' to confirm before quitting the TUI.
    #[serde(default)]
    pub confirm_quit: bool,
    /// Columns to hide from the aggregate table, e.g. ["models", "cached",
    /// "reason"]. Recognized: cached, input, output, reason, convs, tools,
    /// apps, models.
    #[serde(default)]
    pub hidden_columns: Vec<String>,
    /// Accent color for the title, tab bar and selected row: "cyan" | "green"
    /// | "magenta" | "blue" | "red" | "yellow" | "white".
    #[serde(default = "default_accent_color")]
    pub accent_color: String,
    /// Tint each Cost cell by magnitude (dim -> green -> yellow -> red).
    #[serde(default)]
    pub color_costs: bool,
    /// Show the "AGENTIC DEVELOPMENT TOOL ACTIVITY ANALYSIS" header banner.
    #[serde(default = "default_true")]
    pub show_header: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            reverse_sort_default: false,
            hide_empty_periods: false,
            default_view: default_view(),
            default_tab: String::new(),
            confirm_quit: false,
            hidden_columns: Vec::new(),
            accent_color: default_accent_color(),
            color_costs: false,
            show_header: true,
        }
    }
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
            },
            formatting: FormattingConfig {
                number_comma: false,
                number_human: false,
                locale: "en".to_string(),
                decimal_places: 2,
                currency_symbol: default_currency_symbol(),
                cost_decimal_places: default_cost_decimal_places(),
            },
            tui: TuiConfig::default(),
            models: HashMap::new(),
            aliases: HashMap::new(),
        }
    }
}

thread_local! {
    static TEST_CONFIG_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static TEST_STATE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_config_path(path: PathBuf) {
    TEST_CONFIG_PATH.with(|p| *p.borrow_mut() = Some(path));
}

#[cfg(test)]
pub fn set_test_state_path(path: PathBuf) {
    TEST_STATE_PATH.with(|p| *p.borrow_mut() = Some(path));
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
}

#[derive(Debug, Deserialize, Default)]
struct LegacyConfigFile {
    #[serde(default)]
    upload: LegacyUploadConfig,
}

#[derive(Debug, Deserialize, Default)]
struct LegacyUploadConfig {
    last_date_uploaded: Option<i64>,
}

impl UploadState {
    /// Returns the path to the state file, using a test override when running under `cfg(test)`.
    pub fn state_path() -> Result<PathBuf> {
        #[cfg(test)]
        {
            if let Some(path) = TEST_STATE_PATH.with(|p| p.borrow().clone()) {
                return Ok(path);
            }
        }

        let state_root = dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .context("Could not find platform state directory")?;

        Ok(state_root.join("splitrail").join("state.toml"))
    }

    /// Load upload state from the state file.
    ///
    /// If the state file does not exist, attempts to migrate `last_date_uploaded`
    /// from the legacy config location. Falls back to a zero-value default if
    /// neither source is present.
    pub fn load() -> Result<Self> {
        let state_path = Self::state_path()?;
        if state_path.exists() {
            let content = fs::read_to_string(&state_path).context("Failed to read state file")?;
            return toml::from_str(&content).context("Failed to parse state file");
        }

        if let Some(state) = Self::load_legacy_from_config()? {
            if state.last_date_uploaded > 0 {
                state.save()?;
            }
            return Ok(state);
        }

        Ok(Self::default())
    }

    /// Persist the current state to the state file, creating the directory if needed.
    pub fn save(&self) -> Result<()> {
        let state_path = Self::state_path()?;
        if let Some(parent) = state_path.parent() {
            fs::create_dir_all(parent).context("Failed to create state directory")?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize state")?;
        fs::write(&state_path, content).context("Failed to write state file")?;
        Ok(())
    }

    /// Read `last_date_uploaded` from the old `[upload]` section of the config file, if present.
    /// Returns `None` when the config file does not exist or the field is absent.
    fn load_legacy_from_config() -> Result<Option<Self>> {
        let config_path = Config::config_path()?;
        if !config_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&config_path).context("Failed to read config file")?;
        let legacy: LegacyConfigFile =
            toml::from_str(&content).context("Failed to parse config file")?;

        Ok(legacy
            .upload
            .last_date_uploaded
            .map(|last_date_uploaded| Self { last_date_uploaded }))
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
            println!(
                "   TUI Reverse Sort Default: {}",
                config.tui.reverse_sort_default
            );
            println!(
                "   TUI Hide Empty Periods: {}",
                config.tui.hide_empty_periods
            );
            if !config.models.is_empty() {
                println!("   Custom Models: {}", config.models.len());
            }
            if !config.aliases.is_empty() {
                println!("   Custom Aliases: {}", config.aliases.len());
            }
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
        "reverse-sort-default" => {
            let enabled = value
                .parse::<bool>()
                .context("Invalid boolean value. Use 'true' or 'false'")?;
            config.tui.reverse_sort_default = enabled;
        }
        "hide-empty-periods" => {
            let enabled = value
                .parse::<bool>()
                .context("Invalid boolean value. Use 'true' or 'false'")?;
            config.tui.hide_empty_periods = enabled;
        }
        _ => anyhow::bail!("Unknown config key: {}", key),
    }

    config.save(false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PricingStructure;
    use tempfile::TempDir;

    fn setup_test_config() -> (TempDir, PathBuf, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let config_path = dir.path().join(".splitrail.toml");
        let state_path = dir.path().join("state.toml");
        set_test_config_path(config_path.clone());
        set_test_state_path(state_path.clone());
        (dir, config_path, state_path)
    }

    #[test]
    fn test_config_with_custom_models() {
        let toml_str = r#"
[server]
url = "https://custom.example.com"
api_token = "test-token"

[upload]
auto_upload = true
upload_today_only = false
retry_attempts = 5

[formatting]
number_comma = true
number_human = false
locale = "zh"
decimal_places = 4

[models."custom-model"]
pricing = { Flat = { input_per_1m = 10.0, output_per_1m = 20.0 } }
caching = "None"
is_estimated = true

[aliases]
"my-alias" = "custom-model"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();

        assert_eq!(config.server.url, "https://custom.example.com");
        assert!(config.models.contains_key("custom-model"));

        let custom_model = config.models.get("custom-model").unwrap();
        match &custom_model.pricing {
            PricingStructure::Flat {
                input_per_1m,
                output_per_1m,
            } => {
                assert_eq!(*input_per_1m, 10.0);
                assert_eq!(*output_per_1m, 20.0);
            }
            _ => panic!("Expected flat pricing"),
        }

        assert_eq!(config.aliases.get("my-alias").unwrap(), "custom-model");
    }

    #[test]
    fn default_config_round_trip() {
        let (_dir, config_path, _state_path) = setup_test_config();
        // Ensure there is a default config on disk using the CLI helper.
        create_default_config(true).expect("create_default_config");

        let loaded = Config::load()
            .expect("load config")
            .expect("config should exist");

        assert_eq!(loaded.server.url, "https://splitrail.dev");
        assert_eq!(loaded.server.api_token, "");
        assert!(!loaded.upload.auto_upload);
        assert_eq!(loaded.formatting.locale, "en");
        assert!(!loaded.tui.reverse_sort_default);
        assert!(!loaded.tui.hide_empty_periods);

        let saved = fs::read_to_string(config_path).expect("read saved config");
        assert!(
            !saved.contains("last_date_uploaded"),
            "runtime upload state should not be persisted in config"
        );
    }

    #[test]
    fn set_config_value_behaviour() {
        let (_dir, _path, _state_path) = setup_test_config();

        // Ensure base config exists.
        create_default_config(true).expect("create_default_config");

        set_config_value("api-token", "TEST_TOKEN").expect("set api-token");
        set_config_value("auto-upload", "true").expect("set auto-upload");
        set_config_value("upload-today-only", "true").expect("set upload-today-only");
        set_config_value("number-comma", "true").expect("set number-comma");
        set_config_value("number-human", "true").expect("set number-human");
        set_config_value("locale", "de").expect("set locale");
        set_config_value("decimal-places", "3").expect("set decimal-places");
        set_config_value("reverse-sort-default", "true").expect("set reverse-sort-default");
        set_config_value("hide-empty-periods", "true").expect("set hide-empty-periods");

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
        assert!(cfg.tui.reverse_sort_default);
        assert!(cfg.tui.hide_empty_periods);

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

    #[test]
    fn legacy_config_upload_checkpoint_migrates_to_state() {
        let (_dir, config_path, state_path) = setup_test_config();
        fs::write(
            &config_path,
            r#"
[server]
url = "https://splitrail.dev"
api_token = ""

[upload]
auto_upload = false
upload_today_only = false
retry_attempts = 3
last_date_uploaded = 1234

[formatting]
number_comma = false
number_human = false
locale = "en"
decimal_places = 2
"#,
        )
        .expect("write legacy config");

        let state = UploadState::load().expect("load migrated state");
        assert_eq!(state.last_date_uploaded, 1234);

        let saved_state = fs::read_to_string(state_path).expect("read state file");
        assert!(saved_state.contains("last_date_uploaded = 1234"));
    }

    #[test]
    fn config_toml_parses_tui_section() {
        let toml_str = r#"
[server]
url = "https://splitrail.dev"
api_token = ""

[upload]
auto_upload = false
upload_today_only = false
retry_attempts = 3

[formatting]
number_comma = false
number_human = false
locale = "en"
decimal_places = 2

[tui]
reverse_sort_default = true
hide_empty_periods = true
"#;

        let config: Config = toml::from_str(toml_str).expect("parse config");
        assert!(config.tui.reverse_sort_default);
        assert!(config.tui.hide_empty_periods);
    }
}
