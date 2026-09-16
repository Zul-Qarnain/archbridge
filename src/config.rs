use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default = "default_true")]
    pub official: bool,
    #[serde(default = "default_true")]
    pub aur: bool,
    #[serde(default = "default_true")]
    pub flatpak: bool,
    #[serde(default = "default_true")]
    pub appimage: bool,
    #[serde(default = "default_true")]
    pub upstream: bool,
    #[serde(default = "default_true")]
    pub deb: bool,
    #[serde(default = "default_true")]
    pub rpm: bool,

    #[serde(default)]
    pub repos: BTreeMap<String, String>,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            official: true,
            aur: true,
            flatpak: true,
            appimage: true,
            upstream: true,
            deb: true,
            rpm: true,
            repos: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("archbridge").join("config.json");
            }
        }
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home)
                .join(".config")
                .join("archbridge")
                .join("config.json");
        }
        PathBuf::from(".config/archbridge/config.json")
    }

    pub fn load() -> Result<Self, String> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read config file '{:?}': {}", path, e))?;
        let config: Config = serde_json::from_str(&content)
            .map_err(|e| format!("Invalid config file format in '{:?}': {}", path, e))?;
        Ok(config)
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory '{:?}': {}", parent, e))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        fs::write(&path, content)
            .map_err(|e| format!("Failed to write config file '{:?}': {}", path, e))?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<serde_json::Value, String> {
        match key {
            "official" => Ok(serde_json::Value::Bool(self.official)),
            "aur" => Ok(serde_json::Value::Bool(self.aur)),
            "flatpak" => Ok(serde_json::Value::Bool(self.flatpak)),
            "appimage" => Ok(serde_json::Value::Bool(self.appimage)),
            "upstream" => Ok(serde_json::Value::Bool(self.upstream)),
            "deb" => Ok(serde_json::Value::Bool(self.deb)),
            "rpm" => Ok(serde_json::Value::Bool(self.rpm)),
            "all" => Ok(serde_json::to_value(self).unwrap_or_default()),
            k if k.starts_with("repo.") => {
                let repo_name = &k[5..];
                if let Some(val) = self.repos.get(repo_name) {
                    Ok(serde_json::Value::String(val.clone()))
                } else {
                    Err(format!("Unknown repo configuration key '{}'", key))
                }
            }
            _ => Err(format!("Unknown configuration key '{}'", key)),
        }
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "official" => self.official = parse_bool(value)?,
            "aur" => self.aur = parse_bool(value)?,
            "flatpak" => self.flatpak = parse_bool(value)?,
            "appimage" => self.appimage = parse_bool(value)?,
            "upstream" => self.upstream = parse_bool(value)?,
            "deb" => self.deb = parse_bool(value)?,
            "rpm" => self.rpm = parse_bool(value)?,
            k if k.starts_with("repo.") => {
                let repo_name = &k[5..];
                if repo_name.is_empty() {
                    return Err("Repo name cannot be empty".to_string());
                }
                self.repos.insert(repo_name.to_string(), value.to_string());
            }
            _ => return Err(format!("Unknown configuration key '{}'", key)),
        }
        Ok(())
    }

    pub fn is_enabled(&self, source: &str) -> bool {
        match source {
            "official" => self.official,
            "aur" => self.aur,
            "flatpak" => self.flatpak,
            "appimage" => self.appimage,
            "upstream" => self.upstream,
            "deb" => self.deb,
            "rpm" => self.rpm,
            _ => false,
        }
    }
}

fn parse_bool(val: &str) -> Result<bool, String> {
    match val.to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!("Invalid boolean value '{}'", val)),
    }
}
