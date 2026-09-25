use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

    /// Where finished build artifacts are copied. Empty keeps the default: the
    /// per-job build directory. An absolute path only, so a build run from a
    /// different working directory cannot scatter artifacts around the disk.
    #[serde(default)]
    pub output_dir: String,

    /// File this config was read from and must be written back to. Not part of
    /// the payload: it lets a test or a sandboxed run point preferences at a
    /// temporary path instead of the real `~/.config/archbridge/config.json`.
    #[serde(skip, default = "Config::config_path")]
    pub path: PathBuf,
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
            output_dir: String::new(),
            path: Self::config_path(),
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
        Self::load_from(&Self::config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                ..Self::default()
            });
        }
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file '{:?}': {}", path, e))?;
        let mut config: Config = serde_json::from_str(&content)
            .map_err(|e| format!("Invalid config file format in '{:?}': {}", path, e))?;
        config.path = path.to_path_buf();
        Ok(config)
    }

    pub fn save(&self) -> Result<(), String> {
        let path = &self.path;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory '{:?}': {}", parent, e))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        fs::write(path, content)
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
            "output_dir" => Ok(serde_json::Value::String(self.output_dir.clone())),
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
            "output_dir" => {
                let dir = value.trim();
                if !dir.is_empty() && !Path::new(dir).is_absolute() {
                    return Err(format!(
                        "Output directory must be an absolute path, got '{dir}'"
                    ));
                }
                self.output_dir = dir.to_string();
            }
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
