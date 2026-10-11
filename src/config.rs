use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Every key a client may read or write, used to reject typos instead of
/// silently ignoring them.
const KNOWN_KEYS: &[&str] = &[
    "official",
    "aur",
    "flatpak",
    "appimage",
    "upstream",
    "deb",
    "rpm",
    "repos",
    "keep_jobs",
    "output_dir",
];

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

    /// Keep every build's working directory (`.archbridge/jobs/<job>`) instead
    /// of removing the sources, build trees and chroot once the build ends.
    #[serde(default)]
    pub keep_jobs: bool,

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
            keep_jobs: false,
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
        let shown = path.display();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file '{shown}': {e}"))?;
        if content.trim().is_empty() {
            return Err(format!(
                "Config file '{shown}' is empty. Delete it to fall back to the defaults, or write a JSON object such as {{\"aur\": true}}"
            ));
        }
        let value: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            format!(
                "Config file '{shown}' is not valid JSON ({e}). A half-written file is usually the cause: fix or delete it, then re-run `archbridge config get all`"
            )
        })?;

        // An unknown key is a typo that would otherwise be ignored forever, so
        // the preference the user believes they set never takes effect.
        if let Some(object) = value.as_object() {
            let unknown: Vec<&str> = object
                .keys()
                .map(String::as_str)
                .filter(|key| !KNOWN_KEYS.contains(key))
                .collect();
            if !unknown.is_empty() {
                return Err(format!(
                    "Config file '{shown}' has unknown key(s): {}. Known keys are: {}",
                    unknown.join(", "),
                    KNOWN_KEYS.join(", ")
                ));
            }
        }

        let mut config: Config = serde_json::from_value(value).map_err(|e| {
            format!("Invalid config file contents in '{shown}': {e}. Fix or delete the file")
        })?;
        config.path = path.to_path_buf();
        config.validate()?;
        Ok(config)
    }

    /// Re-checks what came off disk: a hand-edited file must not smuggle a
    /// non-HTTPS repository or a path escaping its directory into discovery.
    fn validate(&self) -> Result<(), String> {
        let shown = self.path.display();
        validate_output_dir(&self.output_dir).map_err(|e| {
            format!("Config file '{shown}' is unusable: {e}. Fix or delete the file")
        })?;
        for (name, url) in &self.repos {
            validate_repo_name(name).map_err(|e| {
                format!("Config file '{shown}' is unusable: {e}. Fix or delete the file")
            })?;
            validate_repo_url(url).map_err(|e| {
                format!("Config file '{shown}' is unusable: {e}. Fix or delete the file")
            })?;
        }
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        let path = &self.path;
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        fs::create_dir_all(&parent).map_err(|e| {
            format!(
                "Failed to create config directory '{}': {}",
                parent.display(),
                e
            )
        })?;
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;

        // Write-then-rename: a crash or full disk in the middle of `fs::write`
        // used to leave a truncated file behind, and the next run quietly reset
        // every preference.
        let mut temp = tempfile::NamedTempFile::new_in(&parent)
            .map_err(|e| format!("Failed to create a temporary config file: {e}"))?;
        temp.write_all(content.as_bytes())
            .and_then(|_| temp.flush())
            .map_err(|e| format!("Failed to write config file '{}': {e}", path.display()))?;
        let persisted = temp
            .persist(path)
            .map_err(|e| format!("Failed to save config file '{}': {e}", path.display()))?;
        drop(persisted);
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
            "keep_jobs" => Ok(serde_json::Value::Bool(self.keep_jobs)),
            "output_dir" => Ok(serde_json::Value::String(self.output_dir.clone())),
            "all" => Ok(serde_json::to_value(self).unwrap_or_default()),
            k if k.starts_with("repo.") => {
                let repo_name = &k[5..];
                if repo_name.is_empty() {
                    return Err("Repo name cannot be empty".to_string());
                }
                match self.repos.get(repo_name) {
                    Some(val) => Ok(serde_json::Value::String(val.clone())),
                    None => Err(format!(
                        "Unknown repo configuration key '{key}'; configured repos: {}",
                        if self.repos.is_empty() {
                            "none".to_string()
                        } else {
                            self.repos.keys().cloned().collect::<Vec<_>>().join(", ")
                        }
                    )),
                }
            }
            _ => Err(format!(
                "Unknown configuration key '{key}'; known keys are: {}",
                KNOWN_KEYS.join(", ")
            )),
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
            "keep_jobs" => self.keep_jobs = parse_bool(value)?,
            "output_dir" => {
                let dir = value.trim();
                validate_output_dir(dir)?;
                self.output_dir = dir.to_string();
            }
            k if k.starts_with("repo.") => {
                let repo_name = &k[5..];
                validate_repo_name(repo_name)?;
                let url = value.trim();
                validate_repo_url(url)?;
                self.repos.insert(repo_name.to_string(), url.to_string());
            }
            _ => {
                return Err(format!(
                    "Unknown configuration key '{key}'; known keys are: {}",
                    KNOWN_KEYS.join(", ")
                ))
            }
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
    // Surrounding space and capitalisation are what a person types, not a
    // different value: ` set aur " true "` means true.
    match val.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "y" => Ok(true),
        "false" | "0" | "no" | "off" | "n" => Ok(false),
        _ => Err(format!(
            "Invalid boolean value '{val}'; expected true/false, 1/0, yes/no or on/off"
        )),
    }
}

/// Empty clears the preference; anything else has to be a usable absolute
/// directory that stays inside the filesystem tree the user named.
fn validate_output_dir(dir: &str) -> Result<(), String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return Ok(());
    }
    if dir.contains('\0') {
        return Err("Output directory cannot contain a NUL byte".to_string());
    }
    if dir.len() > 4096 {
        return Err("Output directory path is too long (max 4096 bytes)".to_string());
    }
    if contains_control_chars(dir) {
        return Err("Output directory cannot contain control characters".to_string());
    }
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(format!(
            "Output directory must be an absolute path, got '{dir}'"
        ));
    }
    if dir == "/" {
        return Err("Output directory cannot be the filesystem root".to_string());
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "Output directory '{dir}' must not contain '..'; give the resolved absolute path"
        ));
    }
    Ok(())
}

fn validate_repo_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Repo name cannot be empty".to_string());
    }
    if name.len() > 128 {
        return Err("Repo name is too long (max 128 bytes)".to_string());
    }
    if name
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
    {
        return Err(format!(
            "Repo name '{name}' is invalid; it must start with a letter or digit and use only letters, digits, '.', '_', '+', '-' or '@'"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '@'))
    {
        return Err(format!(
            "Repo name '{name}' is invalid; use letters, digits, '.', '_', '+', '-' or '@'"
        ));
    }
    Ok(())
}

/// A repository override becomes the target of HTTPS requests and of the source
/// a build downloads, so anything weaker than an explicit https: URL is refused.
fn validate_repo_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Repo URL cannot be empty".to_string());
    }
    if url.len() > 2048 {
        return Err("Repo URL is too long (max 2048 bytes)".to_string());
    }
    if url.chars().any(|c| c.is_control()) || url.contains(char::is_whitespace) {
        return Err("Repo URL cannot contain whitespace or control characters".to_string());
    }
    // A query, a fragment or credentials change what is actually fetched, and an
    // override is meant to name a repository, not a redirect.
    if url.contains(['?', '#', '@']) {
        return Err(format!(
            "Repo URL '{url}' must be a plain repository address, without '?', '#' or credentials"
        ));
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("Repo URL '{url}' must include a scheme, e.g. https://host/repo"))?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(format!(
            "Repo URL '{url}' must use https; '{scheme}' is not allowed"
        ));
    }
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        return Err(format!("Repo URL '{url}' has no host"));
    }
    if host.contains(char::is_whitespace) || host.len() > 253 {
        return Err(format!("Repo URL '{url}' has an invalid host"));
    }
    Ok(())
}

fn contains_control_chars(value: &str) -> bool {
    // A newline in a path is how one argument quietly becomes two.
    value.chars().any(|c| c.is_control())
}
