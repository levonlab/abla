//! User configuration: `$XDG_CONFIG_HOME/abla/config.toml`.
//!
//! The file holds the API key, so it is written with mode 0600 inside a
//! 0700 directory. `ABLA_API_KEY` in the environment overrides the stored key.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::providers::Kind;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Provider,
    pub button: Button,
    pub general: General,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Provider {
    /// `mistral`, `openai`, `groq` or `custom`.
    pub name: String,
    /// Base URL up to and including `/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// The chosen model streams text while you speak.
    pub realtime: bool,
    /// ISO 639-1 code, or empty for automatic detection.
    pub language: String,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            name: Kind::Mistral.id().to_string(),
            base_url: Kind::Mistral.default_base_url().to_string(),
            api_key: String::new(),
            model: String::new(),
            realtime: false,
            language: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Button {
    /// Diameter in pixels.
    pub size: u32,
    /// Recording colour, `#rrggbb`.
    pub color: String,
    /// Distance from the right edge of the screen.
    pub margin_right: i32,
    /// Distance from the bottom edge of the screen.
    pub margin_bottom: i32,
}

impl Default for Button {
    fn default() -> Self {
        Self {
            size: 64,
            color: "#d93a3f".to_string(),
            margin_right: 40,
            margin_bottom: 60,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub autostart: bool,
}

pub const SIZES: [u32; 4] = [52, 64, 72, 88];

pub const COLORS: [(&str, &str); 5] = [
    ("Red", "#d93a3f"),
    ("Blue", "#2d7ff0"),
    ("Green", "#2f9e63"),
    ("Amber", "#e08a1e"),
    ("Violet", "#8452d9"),
];

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(dirs::config_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("abla").join("config.toml")
    }

    /// `Ok(None)` when no configuration has been written yet.
    pub fn load() -> Result<Option<Config>> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let config = toml::from_str(&text)
            .with_context(|| format!("could not parse {}", path.display()))?;
        Ok(Some(config))
    }

    pub fn load_or_default() -> Config {
        match Self::load() {
            Ok(Some(config)) => config,
            Ok(None) => Config::default(),
            Err(err) => {
                log::warn!("{err:#}; starting with defaults");
                Config::default()
            }
        }
    }

    /// Writes atomically with private permissions.
    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let dir = path.parent().context("config path has no parent")?;
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// True once a key and a model are set, so the button can work.
    pub fn is_ready(&self) -> bool {
        !self.api_key().is_empty() && !self.provider.model.is_empty()
    }

    pub fn api_key(&self) -> String {
        std::env::var("ABLA_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| self.provider.api_key.clone())
    }

    pub fn kind(&self) -> Kind {
        Kind::from_id(&self.provider.name)
    }
}

/// Parses `#rrggbb` into 0..1 components. Falls back to the default red.
pub fn rgb(color: &str) -> (f64, f64, f64) {
    let hex = color.trim().trim_start_matches('#');
    if hex.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        }
    }
    rgb(Button::default().color.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let mut config = Config::default();
        config.provider.api_key = "k".into();
        config.provider.model = "voxtral-mini-latest".into();
        config.button.size = 72;
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, config);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let back: Config = toml::from_str("[button]\nsize = 88\n").unwrap();
        assert_eq!(back.button.size, 88);
        assert_eq!(back.button.color, Button::default().color);
        assert_eq!(back.provider.name, "mistral");
    }

    #[test]
    fn parses_colours_and_survives_garbage() {
        assert_eq!(rgb("#ffffff"), (1.0, 1.0, 1.0));
        assert_eq!(rgb("000000"), (0.0, 0.0, 0.0));
        assert_eq!(rgb("nonsense"), rgb("#d93a3f"));
    }
}
