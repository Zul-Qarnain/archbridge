use crate::config::Config;
use crate::process::{run_step, Step};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub name: String,
    pub version: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionRow {
    pub source: String,
    pub availability: String, // "available", "unavailable", "unknown", "disabled"
    pub recommended: bool,
    pub reason: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionReport {
    pub target: String,
    pub recommended_source: Option<String>,
    pub decision_table: Vec<DecisionRow>,
}

pub fn search(target: &str, explicit_repo: Option<&str>, config: &Config) -> DecisionReport {
    let sources = [
        "official", "aur", "flatpak", "appimage", "upstream", "deb", "rpm",
    ];
    let mut rows = Vec::new();

    let is_deb_file = target.ends_with(".deb") && Path::new(target).exists();
    let is_rpm_file = target.ends_with(".rpm") && Path::new(target).exists();

    // Determine repository URL if available
    let repo_url = explicit_repo
        .map(|s| s.to_string())
        .or_else(|| config.repos.get(target).cloned());

    for source in sources {
        if !config.is_enabled(source) {
            rows.push(DecisionRow {
                source: source.to_string(),
                availability: "disabled".to_string(),
                recommended: false,
                reason: format!(
                    "Source '{}' is disabled in configuration preferences",
                    source
                ),
                candidates: vec![],
            });
            continue;
        }

        match source {
            "official" => {
                let row = query_official(target);
                rows.push(row);
            }
            "aur" => {
                let row = query_aur(target);
                rows.push(row);
            }
            "flatpak" => {
                let row = query_release_asset(target, repo_url.as_deref(), "flatpak");
                rows.push(row);
            }
            "appimage" => {
                let row = query_release_asset(target, repo_url.as_deref(), "appimage");
                rows.push(row);
            }
            "upstream" => {
                let row = query_upstream_source(target, repo_url.as_deref());
                rows.push(row);
            }
            "deb" => {
                let availability = if is_deb_file {
                    "available"
                } else {
                    "unavailable"
                };
                let reason = if is_deb_file {
                    "Local DEB file detected for inspection"
                } else {
                    "No DEB package specified or file not found"
                };
                let candidates = if is_deb_file {
                    vec![Candidate {
                        name: target.to_string(),
                        version: None,
                        url: Some(target.to_string()),
                        source_url: None,
                        description: Some("Local DEB package file (inspection path)".to_string()),
                    }]
                } else {
                    vec![]
                };
                rows.push(DecisionRow {
                    source: source.to_string(),
                    availability: availability.to_string(),
                    recommended: false,
                    reason: reason.to_string(),
                    candidates,
                });
            }
            "rpm" => {
                let availability = if is_rpm_file {
                    "available"
                } else {
                    "unavailable"
                };
                let reason = if is_rpm_file {
                    "Local RPM file detected for inspection"
                } else {
                    "No RPM package specified or file not found"
                };
                let candidates = if is_rpm_file {
                    vec![Candidate {
                        name: target.to_string(),
                        version: None,
                        url: Some(target.to_string()),
                        source_url: None,
                        description: Some("Local RPM package file (inspection path)".to_string()),
                    }]
                } else {
                    vec![]
                };
                rows.push(DecisionRow {
                    source: source.to_string(),
                    availability: availability.to_string(),
                    recommended: false,
                    reason: reason.to_string(),
                    candidates,
                });
            }
            _ => {}
        }
    }

    // Determine recommendation: first available source in preference order
    let mut recommended_source = None;
    for row in &mut rows {
        if row.availability == "available" && recommended_source.is_none() {
            row.recommended = true;
            recommended_source = Some(row.source.clone());
        }
    }

    DecisionReport {
        target: target.to_string(),
        recommended_source,
        decision_table: rows,
    }
}

fn query_official(target: &str) -> DecisionRow {
    let step = Step::new(
        "pacman",
        vec!["-Ss", target],
        "Query official Arch repositories",
        10,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            let candidates = parse_pacman_ss(&out.stdout, target);
            if candidates.is_empty() {
                DecisionRow {
                    source: "official".to_string(),
                    availability: "unavailable".to_string(),
                    recommended: false,
                    reason: format!("No exact/matching official package found for '{}'", target),
                    candidates: vec![],
                }
            } else {
                DecisionRow {
                    source: "official".to_string(),
                    availability: "available".to_string(),
                    recommended: false,
                    reason: format!(
                        "Found {} candidate(s) in official Arch repositories",
                        candidates.len()
                    ),
                    candidates,
                }
            }
        }
        Ok(_) => DecisionRow {
            source: "official".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("No official repository package matched '{}'", target),
            candidates: vec![],
        },
        Err(e) => DecisionRow {
            source: "official".to_string(),
            availability: "unknown".to_string(),
            recommended: false,
            reason: format!("Failed to query pacman sync db: {}", e),
            candidates: vec![],
        },
    }
}

fn parse_pacman_ss(output: &str, target: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if !line.is_empty() && line.contains('/') {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let pkg_full = parts[0]; // e.g. core/bash or extra/bash
                let version = parts[1];
                let pkg_name = pkg_full.split('/').nth(1).unwrap_or(pkg_full);
                let desc = if i + 1 < lines.len() {
                    lines[i + 1].trim().to_string()
                } else {
                    String::new()
                };
                if pkg_name.eq_ignore_ascii_case(target) || pkg_name.contains(target) {
                    candidates.push(Candidate {
                        name: pkg_name.to_string(),
                        version: Some(version.to_string()),
                        url: None,
                        source_url: None,
                        description: if desc.is_empty() { None } else { Some(desc) },
                    });
                }
            }
        }
        i += 1;
    }
    candidates
}

fn query_aur(target: &str) -> DecisionRow {
    let url = format!("https://aur.archlinux.org/rpc/v5/search/{}", target);
    let step = Step::new(
        "curl",
        vec!["-sSL", "-m", "10", &url],
        "Query AUR RPC v5",
        10,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&out.stdout) {
                if let Some(results) = val.get("results").and_then(|r| r.as_array()) {
                    let mut candidates = Vec::new();
                    for item in results {
                        let name = item
                            .get("Name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let ver = item
                            .get("Version")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let desc = item
                            .get("Description")
                            .and_then(|d| d.as_str())
                            .map(|s| s.to_string());
                        let aur_url = item
                            .get("URL")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string());

                        if name.eq_ignore_ascii_case(target) || name.contains(target) {
                            candidates.push(Candidate {
                                name,
                                version: ver,
                                source_url: aur_url.clone(),
                                url: aur_url,
                                description: desc,
                            });
                        }
                    }

                    if candidates.is_empty() {
                        return DecisionRow {
                            source: "aur".to_string(),
                            availability: "unavailable".to_string(),
                            recommended: false,
                            reason: format!("No matching packages found in AUR for '{}'", target),
                            candidates: vec![],
                        };
                    } else {
                        return DecisionRow {
                            source: "aur".to_string(),
                            availability: "available".to_string(),
                            recommended: false,
                            reason: format!("Found {} package(s) in AUR", candidates.len()),
                            candidates,
                        };
                    }
                }
            }
            DecisionRow {
                source: "aur".to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: "AUR RPC returned invalid JSON or empty result".to_string(),
                candidates: vec![],
            }
        }
        Ok(_) => DecisionRow {
            source: "aur".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("AUR RPC query returned no results for '{}'", target),
            candidates: vec![],
        },
        Err(e) => DecisionRow {
            source: "aur".to_string(),
            availability: "unknown".to_string(),
            recommended: false,
            reason: format!("Failed to reach AUR RPC API: {}", e),
            candidates: vec![],
        },
    }
}

fn query_release_asset(target: &str, repo_url: Option<&str>, asset_type: &str) -> DecisionRow {
    let repo_url_str = match repo_url {
        Some(url) => url,
        None => {
            return DecisionRow {
                source: asset_type.to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: format!(
                    "No repository URL known for target '{}'; pass --repo <url>",
                    target
                ),
                candidates: vec![],
            };
        }
    };

    let api_url = match parse_github_repo(repo_url_str) {
        Some((owner, repo)) => format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            owner, repo
        ),
        None => {
            return DecisionRow {
                source: asset_type.to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: format!(
                    "Repository URL '{}' is not a recognized GitHub release host",
                    repo_url_str
                ),
                candidates: vec![],
            };
        }
    };

    let step = Step::new(
        "curl",
        vec!["-sSL", "-m", "10", "-H", "User-Agent: archbridge", &api_url],
        "Query GitHub release assets",
        10,
    );
    match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&out.stdout) {
                if let Some(assets) = val.get("assets").and_then(|a| a.as_array()) {
                    let mut candidates = Vec::new();
                    for asset in assets {
                        let name = asset.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let download_url = asset
                            .get("browser_download_url")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string());

                        let matches = match asset_type {
                            "flatpak" => {
                                name.ends_with(".flatpakref") || name.ends_with(".flatpak")
                            }
                            "appimage" => {
                                name.ends_with(".AppImage") || name.ends_with(".appimage")
                            }
                            _ => false,
                        };

                        if matches {
                            candidates.push(Candidate {
                                name: name.to_string(),
                                version: val
                                    .get("tag_name")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string()),
                                url: download_url,
                                source_url: Some(repo_url_str.to_string()),
                                description: Some(format!("Release asset from {}", repo_url_str)),
                            });
                        }
                    }

                    if !candidates.is_empty() {
                        return DecisionRow {
                            source: asset_type.to_string(),
                            availability: "available".to_string(),
                            recommended: false,
                            reason: format!(
                                "Found {} {} release asset(s)",
                                candidates.len(),
                                asset_type
                            ),
                            candidates,
                        };
                    }
                }
            }
            DecisionRow {
                source: asset_type.to_string(),
                availability: "unavailable".to_string(),
                recommended: false,
                reason: format!(
                    "No {} asset found in latest release for repository",
                    asset_type
                ),
                candidates: vec![],
            }
        }
        Ok(_) => DecisionRow {
            source: asset_type.to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("Could not fetch release metadata from {}", api_url),
            candidates: vec![],
        },
        Err(e) => DecisionRow {
            source: asset_type.to_string(),
            availability: "unknown".to_string(),
            recommended: false,
            reason: format!("Failed to query release API: {}", e),
            candidates: vec![],
        },
    }
}

fn query_upstream_source(target: &str, repo_url: Option<&str>) -> DecisionRow {
    let repo_url_str = match repo_url {
        Some(url) => url,
        None => {
            if target.starts_with("https://github.com/")
                || target.starts_with("https://gitlab.com/")
            {
                target
            } else {
                return query_github_repositories(target);
            }
        }
    };

    if let Some((owner, repo)) = parse_github_repo(repo_url_str) {
        let api_url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            owner, repo
        );
        let step = Step::new(
            "curl",
            vec!["-sSL", "-m", "10", "-H", "User-Agent: archbridge", &api_url],
            "Query GitHub release source",
            10,
        );
        if let Ok(out) = run_step(&step, None) {
            if out.exit_code == 0 && !out.stdout.trim().is_empty() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&out.stdout) {
                    let tag_name = val
                        .get("tag_name")
                        .and_then(|t| t.as_str())
                        .unwrap_or("latest");
                    let tarball_url = val
                        .get("tarball_url")
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            format!(
                                "https://github.com/{}/{}/archive/refs/tags/{}.tar.gz",
                                owner, repo, tag_name
                            )
                        });

                    return DecisionRow {
                        source: "upstream".to_string(),
                        availability: "available".to_string(),
                        recommended: false,
                        reason: format!(
                            "Found GitHub release source tarball for tag '{}'",
                            tag_name
                        ),
                        candidates: vec![Candidate {
                            name: repo.clone(),
                            version: Some(tag_name.trim_start_matches('v').to_string()),
                            url: Some(tarball_url),
                            source_url: Some(repo_url_str.to_string()),
                            description: Some(format!(
                                "GitHub source release for {}/{}",
                                owner, repo
                            )),
                        }],
                    };
                }
            }
        }
    }

    DecisionRow {
        source: "upstream".to_string(),
        availability: "unavailable".to_string(),
        recommended: false,
        reason: format!(
            "Could not resolve upstream release source for '{}'",
            repo_url_str
        ),
        candidates: vec![],
    }
}

fn query_github_repositories(target: &str) -> DecisionRow {
    let query = target
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || c.is_ascii_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("+");
    if query.is_empty() {
        return DecisionRow {
            source: "upstream".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: "GitHub search requires a package name or repository URL".to_string(),
            candidates: vec![],
        };
    }

    let api_url = format!(
        "https://api.github.com/search/repositories?q={}&per_page=5",
        query
    );
    let step = Step::new(
        "curl",
        vec!["-sSL", "-m", "10", "-H", "User-Agent: archbridge", &api_url],
        "Search GitHub repositories",
        10,
    );
    let output = match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 => out.stdout,
        _ => String::new(),
    };
    let value = match serde_json::from_str::<serde_json::Value>(&output) {
        Ok(value) => value,
        Err(_) => {
            return DecisionRow {
                source: "upstream".to_string(),
                availability: "unknown".to_string(),
                recommended: false,
                reason: "GitHub repository search returned no usable response".to_string(),
                candidates: vec![],
            };
        }
    };

    let normalized_target = target.to_ascii_lowercase().replace(['-', '_', ' '], "");
    let candidates = value
        .get("items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            let normalized_name = name.to_ascii_lowercase().replace(['-', '_', ' '], "");
            if !normalized_name.contains(&normalized_target)
                && !normalized_target.contains(&normalized_name)
            {
                return None;
            }
            let full_name = item.get("full_name")?.as_str()?;
            let branch = item
                .get("default_branch")
                .and_then(|branch| branch.as_str())
                .unwrap_or("main");
            let html_url = item.get("html_url")?.as_str()?.to_string();
            Some(Candidate {
                name: full_name.to_string(),
                version: None,
                url: Some(format!(
                    "https://github.com/{}/archive/refs/heads/{}.tar.gz",
                    full_name, branch
                )),
                source_url: Some(html_url),
                description: item
                    .get("description")
                    .and_then(|description| description.as_str())
                    .map(|description| description.to_string()),
            })
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        DecisionRow {
            source: "upstream".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: format!("No matching GitHub repository found for '{}'", target),
            candidates,
        }
    } else {
        DecisionRow {
            source: "upstream".to_string(),
            availability: "available".to_string(),
            recommended: false,
            reason: format!(
                "Found {} matching GitHub repository/repositories",
                candidates.len()
            ),
            candidates,
        }
    }
}

pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    if let Some(pos) = trimmed.find("github.com/") {
        let path = &trimmed[pos + "github.com/".len()..];
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }
    None
}
