use crate::config::Config;
use crate::process::{run_step, ProcessOutput, Step};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::Path;

/// Everything discovery does to the outside world goes through a probe, so a
/// test can hand back canned pacman/AUR/GitHub output instead of running a
/// program or reaching the network.
pub trait Probe {
    fn query(&self, step: &Step) -> Result<ProcessOutput, String>;
}

/// The production probe: run the step as a bounded child process.
pub struct ProcessProbe;

impl Probe for ProcessProbe {
    fn query(&self, step: &Step) -> Result<ProcessOutput, String> {
        run_step(step, None)
    }
}

/// Longest display string taken from a foreign source. A description is read by
/// a human in a review, not stored as data.
const DISPLAY_BYTES: usize = 512;
const URL_BYTES: usize = 2048;
/// Candidate cap per source: enough to choose from, far below what a hostile
/// mirror can return in one response.
const MAX_CANDIDATES: usize = 25;

/// Bytes one discovery response may occupy. A search answer is a few hundred
/// records of text; anything larger is a mirror trying to fill the daemon's
/// memory, and `curl` is the only place the transfer can be stopped cheaply.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Build the one `curl` invocation shape discovery uses. `head` carries the
/// caller's own switches (`-fsSL` for a body, `-sSIL` to ask a URL about
/// itself), and what every request adds is a ceiling on the response and a
/// refusal to follow a redirect onto a weaker transport than it started on —
/// an https answer redirected to http can be rewritten by anyone on the path.
fn curl_step(head: &str, extra: &[&str], url: &str, purpose: &str, timeout: u64) -> Step {
    let mut args: Vec<String> = vec![head.to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    args.extend([
        "--proto-redir".to_string(),
        "=https".to_string(),
        "--max-filesize".to_string(),
        MAX_RESPONSE_BYTES.to_string(),
        "-m".to_string(),
        timeout.to_string(),
        url.to_string(),
    ]);
    Step::new("curl", args, purpose, timeout)
}

/// A search target is also a command argument and a URL segment, so it has a
/// ceiling of its own.
fn valid_target(target: &str) -> bool {
    let trimmed = target.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 256
        && !trimmed.contains('\0')
        && !trimmed.chars().any(char::is_control)
}

/// Strip what a terminal would act on and what no reviewer can read: control
/// sequences, and text longer than a screen.
fn sanitize_display(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(DISPLAY_BYTES));
    for ch in value.chars() {
        if ch.is_control() {
            continue;
        }
        if out.chars().count() >= DISPLAY_BYTES {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

fn sanitize_opt(value: Option<&str>) -> Option<String> {
    let cleaned = value.map(sanitize_display).unwrap_or_default();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A URL a candidate offers must be fetchable and unambiguous: http(s) only,
/// no control characters, no embedded whitespace that would split an argument.
fn sanitize_url(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() || raw.len() > URL_BYTES {
        return None;
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (scheme, rest) = raw.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return None;
    }
    if rest.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

/// A package name that will be used to build a request path or a pacman
/// selector. Anything outside the package-name alphabet is data, not a name.
fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && name != "."
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '@'))
}

/// Whether a source's package is the thing the user typed for. Both the pacman
/// sync database and the AUR answer case-insensitively, so matching has to as
/// well: a case-sensitive substring test drops `MyHtop` from an `htop` search.
fn name_matches(name: &str, target: &str) -> bool {
    name.eq_ignore_ascii_case(target) || name.to_lowercase().contains(&target.to_lowercase())
}

/// Largest version ordering this function will attempt. alpm reads whole digit
/// runs as numbers; comparing them by shape keeps a 200-digit response from
/// becoming an overflow, and a run longer than a real version is data.
const MAX_VERSION_BYTES: usize = 256;

/// alpm/RPM version ordering: alternating numeric and alphabetic chunks, where a
/// numeric chunk outranks an alphabetic one, `~` sorts before anything (including
/// the end of the string), a leading `epoch:` wins over everything else, and a
/// longer version breaks a tie. This is what separates `1.10` from `1.9` —
/// lexicographic text order puts `1.9` first, and a review that ranks the older
/// release higher recommends the wrong package.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let (left_epoch, left_rest) = split_version_epoch(left);
    let (right_epoch, right_rest) = split_version_epoch(right);
    match left_epoch.cmp(&right_epoch) {
        Ordering::Equal => {}
        order => return order,
    }
    compare_chunks(&version_chunks(left_rest), &version_chunks(right_rest))
}

fn split_version_epoch(raw: &str) -> (u64, &str) {
    match raw.split_once(':') {
        Some((prefix, rest))
            if !prefix.is_empty()
                && prefix.len() <= 16
                && prefix.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (prefix.parse().unwrap_or(0), rest)
        }
        _ => (0, raw),
    }
}

#[derive(Debug, PartialEq)]
enum VersionChunk<'a> {
    Numeric(&'a str),
    Alpha(&'a str),
    Tilde,
}

fn version_chunks(raw: &str) -> Vec<VersionChunk<'_>> {
    let raw = if raw.len() > MAX_VERSION_BYTES {
        // A ceiling on the work a response can cause, never a cut through the
        // middle of a character.
        let mut end = MAX_VERSION_BYTES;
        while end > 0 && !raw.is_char_boundary(end) {
            end -= 1;
        }
        &raw[..end]
    } else {
        raw
    };
    let bytes = raw.as_bytes();
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'0'..=b'9' => {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                chunks.push(VersionChunk::Numeric(&raw[start..i]));
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                chunks.push(VersionChunk::Alpha(&raw[start..i]));
            }
            b'~' => {
                i += 1;
                chunks.push(VersionChunk::Tilde);
            }
            // Any other byte is a separator: `1.0-2` and `1.0_2` both chunk as
            // 1, 0, 2, which is how a pkgrel participates in the ordering.
            _ => i += 1,
        }
    }
    chunks
}

fn compare_chunks(left: &[VersionChunk<'_>], right: &[VersionChunk<'_>]) -> Ordering {
    for pair in left.iter().zip(right.iter()) {
        let order = match pair {
            (VersionChunk::Tilde, VersionChunk::Tilde) => Ordering::Equal,
            (VersionChunk::Tilde, _) => Ordering::Less,
            (_, VersionChunk::Tilde) => Ordering::Greater,
            (VersionChunk::Numeric(a), VersionChunk::Numeric(b)) => compare_digit_runs(a, b),
            (VersionChunk::Numeric(_), VersionChunk::Alpha(_)) => Ordering::Greater,
            (VersionChunk::Alpha(_), VersionChunk::Numeric(_)) => Ordering::Less,
            (VersionChunk::Alpha(a), VersionChunk::Alpha(b)) => a.cmp(b),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    // A tie over every shared chunk: the version that continues is the newer
    // one, unless what continues is a pre-release marker, which sorts before the
    // version it is a preview of.
    match (left.get(right.len()), right.get(left.len())) {
        (Some(VersionChunk::Tilde), None) => Ordering::Less,
        (None, Some(VersionChunk::Tilde)) => Ordering::Greater,
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        _ => Ordering::Equal,
    }
}

fn compare_digit_runs(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

/// A stated version is worth more than none, and a newer one is worth more than
/// an older one — which is the whole of choosing between two records for the
/// same package.
fn version_is_newer(candidate: Option<&str>, current: Option<&str>) -> bool {
    match (candidate, current) {
        (Some(newer), Some(current)) => compare_versions(newer, current) == Ordering::Greater,
        (Some(_), None) => true,
        _ => false,
    }
}

/// One source can report the same package twice: a mirror repeats a record, a
/// custom repository lists the same name in two databases, and a release can
/// carry an asset under the same name twice. A review of duplicates reads as
/// choice where there is none, so each name/repository pair is kept once — the
/// newest version of it.
fn dedupe_candidates(candidates: &mut Vec<Candidate>) {
    let mut kept: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates.drain(..) {
        match kept.iter_mut().find(|existing| {
            existing.name.eq_ignore_ascii_case(&candidate.name)
                && existing.repository == candidate.repository
        }) {
            None => kept.push(candidate),
            Some(existing)
                if version_is_newer(candidate.version.as_deref(), existing.version.as_deref()) =>
            {
                *existing = candidate;
            }
            Some(_) => {}
        }
    }
    *candidates = kept;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub name: String,
    pub version: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    pub description: Option<String>,
    /// Repository the source itself names (`core`, `extra`, `AUR`).
    #[serde(default)]
    pub repository: Option<String>,
    /// Maintainer or packager as the source states it.
    #[serde(default)]
    pub maintainer: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    /// Size string exactly as the source reports it.
    #[serde(default)]
    pub size: Option<String>,
    /// AUR's own "this may no longer build" marker, set to the date it was
    /// flagged. It changes which source is safest, so it must reach the UI.
    #[serde(default)]
    pub out_of_date: Option<String>,
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
    search_with(target, explicit_repo, config, &ProcessProbe)
}

pub fn search_with(
    target: &str,
    explicit_repo: Option<&str>,
    config: &Config,
    probe: &dyn Probe,
) -> DecisionReport {
    let sources = [
        "official", "aur", "flatpak", "appimage", "upstream", "deb", "rpm",
    ];
    let mut rows = Vec::new();

    // An empty or control-character target is not a query: `pacman -Ss ""` and
    // `?q=` style searches return the whole universe, which is a data dump and a
    // great deal of needless work.
    if !valid_target(target) {
        return DecisionReport {
            target: target.to_string(),
            recommended_source: None,
            decision_table: sources
                .iter()
                .map(|source| DecisionRow {
                    source: source.to_string(),
                    availability: "unavailable".to_string(),
                    recommended: false,
                    reason: if target.trim().is_empty() {
                        "No search target was given".to_string()
                    } else {
                        "The search target contains characters that cannot be queried".to_string()
                    },
                    candidates: vec![],
                })
                .collect(),
        };
    }
    let target = target.trim();

    // The inspector and the importer both decide by lower-cased suffix, so this
    // has to as well — and a directory called `tool.deb` is not a package.
    let folded = target.to_ascii_lowercase();
    let is_deb_file = folded.ends_with(".deb") && Path::new(target).is_file();
    let is_rpm_file = folded.ends_with(".rpm") && Path::new(target).is_file();

    // Determine repository URL if available
    let repo_url = explicit_repo
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
                let row = query_official(target, probe);
                rows.push(row);
            }
            "aur" => {
                let row = query_aur(target, probe);
                rows.push(row);
            }
            "flatpak" => {
                let row = query_release_asset(target, repo_url.as_deref(), "flatpak", probe);
                rows.push(row);
            }
            "appimage" => {
                let row = query_release_asset(target, repo_url.as_deref(), "appimage", probe);
                rows.push(row);
            }
            "upstream" => {
                let row = query_upstream_source(target, repo_url.as_deref(), probe);
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
                        ..Default::default()
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
                        ..Default::default()
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

    // Determine recommendation: first available source in preference order.
    // DEB and RPM are data-only formats that are never installed, so an available
    // foreign file must not become the recommended action.
    let mut recommended_source = None;
    for row in &mut rows {
        if matches!(row.source.as_str(), "deb" | "rpm") {
            continue;
        }
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

fn query_official(target: &str, probe: &dyn Probe) -> DecisionRow {
    let step = Step::new(
        "pacman",
        vec!["-Ss", target],
        "Query official Arch repositories",
        10,
    );
    match probe.query(&step) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            let mut candidates = parse_pacman_ss(&out.stdout, target);
            enrich_official(&mut candidates, probe);
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

/// `pacman -Ss` prints a two-line record per package: `repo/name version` then
/// the description. Neither line is trusted: a name outside the package alphabet
/// is data, not a candidate, and a "description" that is itself a package header
/// means the record ran out before one was printed.
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
                let mut segments = pkg_full.splitn(2, '/');
                let repository = segments.next().unwrap_or_default();
                let pkg_name = segments.next().unwrap_or_default();
                if !pkg_name.is_empty() && is_package_name(pkg_name) && !repository.is_empty() {
                    let desc = match lines.get(i + 1) {
                        // The next record's header is not this package's summary.
                        Some(next) if !is_package_header(next) && !next.trim().is_empty() => {
                            sanitize_opt(Some(next.trim()))
                        }
                        _ => None,
                    };
                    if name_matches(pkg_name, target) {
                        candidates.push(Candidate {
                            name: pkg_name.to_string(),
                            version: sanitize_opt(Some(version)),
                            url: None,
                            source_url: None,
                            description: desc,
                            repository: sanitize_opt(Some(repository)),
                            ..Default::default()
                        });
                        if candidates.len() >= MAX_CANDIDATES {
                            break;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    dedupe_candidates(&mut candidates);
    rank_candidates(&mut candidates, target);
    candidates
}

/// A line shaped like `repo/name 1.2-3`, which starts a new `-Ss` record.
fn is_package_header(line: &str) -> bool {
    let line = line.trim();
    match line.split_once(char::is_whitespace) {
        Some((left, version)) => {
            left.contains('/')
                && is_package_name(left.split('/').nth(1).unwrap_or_default())
                && is_package_name(version)
        }
        None => false,
    }
}

/// Exact matches first, then names that start with the target, then the rest:
/// "found 25 candidates" is useless if the package typed by name is last.
fn rank_candidates(candidates: &mut [Candidate], target: &str) {
    let score = |name: &str| -> u8 {
        if name.eq_ignore_ascii_case(target) {
            0
        } else if name
            .to_ascii_lowercase()
            .starts_with(&target.to_ascii_lowercase())
        {
            1
        } else {
            2
        }
    };
    candidates.sort_by_key(|candidate| (score(&candidate.name), candidate.name.clone()));
}

/// `pacman -Ss` yields only names, versions and one-line descriptions, so the
/// detail panel used to fill licence, architecture, size and maintainer with
/// invented text. `-Si` reports all of them from the sync database, and only the
/// leading candidates are worth the extra local lookups.
fn enrich_official(candidates: &mut [Candidate], probe: &dyn Probe) {
    for candidate in candidates.iter_mut().take(3) {
        let selector = match &candidate.repository {
            Some(repo) if !repo.is_empty() => format!("{}/{}", repo, candidate.name),
            _ => candidate.name.clone(),
        };
        // The selector is a pacman argument built from database text; anything
        // that is not a name/repo pair is not worth asking about.
        if !is_package_name(&candidate.name)
            || candidate
                .repository
                .as_deref()
                .is_some_and(|repo| !is_package_name(repo))
        {
            continue;
        }
        let step = Step::new(
            "pacman",
            vec!["-Si", "--color=never", &selector],
            "Read official package metadata",
            10,
        );
        let Ok(out) = probe.query(&step) else {
            continue;
        };
        if out.exit_code != 0 {
            continue;
        }
        let fields = parse_pacman_si(&out.stdout);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, value)| sanitize_opt(Some(value)))
        };
        if candidate.version.is_none() {
            candidate.version = get("Version");
        }
        candidate.repository = get("Repository").or_else(|| candidate.repository.clone());
        candidate.architecture = get("Architecture");
        candidate.license = get("Licences").or_else(|| get("Licenses"));
        candidate.size = get("Installed Size").or_else(|| get("Download Size"));
        candidate.maintainer = get("Maintainer")
            .filter(|m| !m.eq_ignore_ascii_case("Unknown Packager"))
            .or_else(|| get("Packager"));
        if candidate.url.is_none() {
            candidate.url = sanitize_url(get("URL").as_deref());
        }
        if candidate.description.is_none() {
            candidate.description = get("Description");
        }
    }
}

/// Parse `pacman -Si` "Key : Value" records, rejoining the values pacman wraps
/// onto indented continuation lines.
fn parse_pacman_si(output: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = output.lines().collect();
    let mut fields = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((key, value)) = lines[i].split_once(" : ") {
            let mut values = vec![value.trim().to_string()];
            let mut j = i + 1;
            while j < lines.len() && lines[j].starts_with("    ") {
                values.push(lines[j].trim().to_string());
                j += 1;
            }
            let joined = values
                .into_iter()
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.is_empty() {
                fields.push((key.trim().to_string(), joined));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    fields
}

/// Percent-encode a value destined for a URL path or query segment. A search
/// target is user input: an unencoded space, `?`, `#` or `..` silently changes
/// which resource the request actually reaches.
fn encode_url_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

/// Turn one AUR RPC v5 body into candidates. A mirror or a compromised response
/// can name anything, so a package is only listed when its name is a package
/// name and the URLs it offers are fetchable; everything else is dropped whole.
fn parse_aur_results(body: &str, target: &str) -> Option<Vec<Candidate>> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;
    let results = val.get("results").and_then(|r| r.as_array())?;
    let mut candidates = Vec::new();
    for item in results {
        let name = item.get("Name").and_then(|n| n.as_str()).unwrap_or("");
        if !is_package_name(name) {
            continue;
        }
        if !name_matches(name, target) {
            continue;
        }
        // `URLPath` is the AUR git snapshot holding the PKGBUILD — the actual
        // thing a build would consume, so it is only usable as a path on the
        // AUR host, never as an absolute URL of its own choosing.
        let snapshot_url = item
            .get("URLPath")
            .and_then(|u| u.as_str())
            .and_then(aur_snapshot_url);
        candidates.push(Candidate {
            name: name.to_string(),
            version: sanitize_opt(item.get("Version").and_then(|v| v.as_str())),
            url: sanitize_url(item.get("URL").and_then(|u| u.as_str())),
            source_url: snapshot_url.clone(),
            description: sanitize_opt(item.get("Description").and_then(|d| d.as_str())),
            repository: Some("AUR".to_string()),
            maintainer: sanitize_opt(item.get("Maintainer").and_then(|m| m.as_str())),
            out_of_date: sanitize_opt(item.get("OutOfDate").and_then(|o| o.as_str())),
            license: sanitize_opt(item.get("License").and_then(|l| l.as_str())),
            ..Default::default()
        });
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }
    dedupe_candidates(&mut candidates);
    rank_candidates(&mut candidates, target);
    Some(candidates)
}

/// An AUR `URLPath` is a path on the AUR host. A value that is absolute, escapes
/// with `..`, or starts a new authority (`//host`) would be sent somewhere else.
fn aur_snapshot_url(path: &str) -> Option<String> {
    let path = path.trim();
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\0') {
        return None;
    }
    if path
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return None;
    }
    let full = format!("https://aur.archlinux.org{path}");
    sanitize_url(Some(&full))
}

fn query_aur(target: &str, probe: &dyn Probe) -> DecisionRow {
    let url = format!(
        "https://aur.archlinux.org/rpc/v5/search/{}",
        encode_url_segment(target)
    );
    let step = curl_step("-fsSL", &[], &url, "Query AUR RPC v5", 10);
    match probe.query(&step) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            match parse_aur_results(&out.stdout, target) {
                Some(candidates) if !candidates.is_empty() => DecisionRow {
                    source: "aur".to_string(),
                    availability: "available".to_string(),
                    recommended: false,
                    reason: format!("Found {} package(s) in AUR", candidates.len()),
                    candidates,
                },
                Some(_) => DecisionRow {
                    source: "aur".to_string(),
                    availability: "unavailable".to_string(),
                    recommended: false,
                    reason: format!("No matching packages found in AUR for '{}'", target),
                    candidates: vec![],
                },
                None => DecisionRow {
                    source: "aur".to_string(),
                    availability: "unavailable".to_string(),
                    recommended: false,
                    reason: "AUR RPC returned invalid JSON or empty result".to_string(),
                    candidates: vec![],
                },
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

/// Whether a release asset name ends in this extension, whatever case the
/// uploader used: the same payload is published as `.AppImage`, `.appimage` and
/// `.APPIMAGE`, and matching one spelling silently loses the candidate.
fn ends_extension(name: &str, extension: &str) -> bool {
    name.to_ascii_lowercase().ends_with(extension)
}

/// The architecture a release asset is built for, as far as its name says.
/// Names are free text, so "the name says nothing" is a different answer from
/// "the name says arm64", and only the second one rules the asset out.
fn asset_architecture(name: &str) -> Option<&'static str> {
    // Splitting on every non-alphanumeric character folds `x86_64`, `x86-64` and
    // `x86.64` to the same token sequence, which is then matched as whole
    // tokens so `arm` inside `charmel` cannot decide anything.
    let flat = name
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let padded = format!("-{flat}-");
    let names_any_of = |aliases: &[&str]| {
        aliases
            .iter()
            .any(|alias| padded.contains(&format!("-{alias}-")))
    };
    if names_any_of(&["x86-64", "x86_64", "amd64", "x64"]) {
        Some("x86_64")
    } else if names_any_of(&["aarch64", "arm64"]) {
        Some("aarch64")
    } else if names_any_of(&[
        "i386", "i486", "i586", "i686", "386", "586", "686", "x86-32", "x86", "ia32", "win32",
    ]) {
        Some("i686")
    } else if names_any_of(&[
        "arm", "armhf", "armel", "armv5", "armv6", "armv7", "armv7l", "armv8",
    ]) {
        // Arch has no 32-bit ARM repository, so these are named, wrong, and not
        // the same as being silent about the architecture.
        Some("armv7")
    } else if names_any_of(&[
        "riscv64",
        "ppc64le",
        "ppc64",
        "powerpc",
        "s390x",
        "mips64",
        "mips",
        "loongarch64",
        "sparc64",
    ]) {
        Some("other")
    } else {
        None
    }
}

/// What an asset has to be built for to run here. The compiled target, not a
/// string a response can talk its way into.
fn host_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "i686",
        other => other,
    }
}

fn query_release_asset(
    target: &str,
    repo_url: Option<&str>,
    asset_type: &str,
    probe: &dyn Probe,
) -> DecisionRow {
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

    let step = curl_step(
        "-fsSL",
        &["-H", "User-Agent: archbridge"],
        &api_url,
        "Query GitHub release assets",
        10,
    );
    match probe.query(&step) {
        Ok(out) if out.exit_code == 0 && !out.stdout.trim().is_empty() => {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&out.stdout) {
                if let Some(assets) = val.get("assets").and_then(|a| a.as_array()) {
                    let mut candidates = Vec::new();
                    for asset in assets {
                        let name = match asset.get("name").and_then(|n| n.as_str()) {
                            Some(name) if !name.trim().is_empty() => sanitize_display(name),
                            _ => continue,
                        };
                        // An asset with no fetchable HTTPS location is a name in a
                        // list, not something a build can be pointed at: the engine
                        // only accepts an HTTPS source, and a plain http download
                        // can be replaced on the wire.
                        let download_url = match sanitize_url(
                            asset.get("browser_download_url").and_then(|u| u.as_str()),
                        )
                        .filter(|url| url.starts_with("https://"))
                        {
                            Some(url) => url,
                            None => continue,
                        };

                        // A release asset is a prebuilt binary for one machine.
                        // An asset whose name states an architecture states which
                        // machine it runs on, and offering one built for another
                        // architecture promises a program that cannot start.
                        let architecture = asset_architecture(&name);
                        if architecture.is_some_and(|arch| arch != host_architecture()) {
                            continue;
                        }

                        let matches = match asset_type {
                            "flatpak" => {
                                ends_extension(&name, ".flatpakref")
                                    || ends_extension(&name, ".flatpak")
                            }
                            "appimage" => ends_extension(&name, ".appimage"),
                            _ => false,
                        };

                        if matches {
                            candidates.push(Candidate {
                                name,
                                version: sanitize_opt(val.get("tag_name").and_then(|t| t.as_str())),
                                url: Some(download_url),
                                source_url: Some(repo_url_str.to_string()),
                                description: Some(format!("Release asset from {}", repo_url_str)),
                                architecture: architecture.map(str::to_string),
                                ..Default::default()
                            });
                            if candidates.len() >= MAX_CANDIDATES {
                                break;
                            }
                        }
                    }

                    dedupe_candidates(&mut candidates);
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

fn query_upstream_source(target: &str, repo_url: Option<&str>, probe: &dyn Probe) -> DecisionRow {
    let repo_url_str = match repo_url {
        Some(url) => url,
        None => {
            if target.starts_with("https://github.com/")
                || target.starts_with("https://gitlab.com/")
            {
                target
            } else {
                return query_github_repositories(target, probe);
            }
        }
    };
    let mut notes: Vec<String> = Vec::new();

    if let Some((owner, repo)) = parse_github_repo(repo_url_str) {
        // A tagged release is the reproducible choice, so it is tried first; every
        // step that fails records why, instead of a later message guessing.
        let release =
            match github_api_json(&format!("/repos/{}/{}/releases/latest", owner, repo), probe) {
                Ok(val) => val
                    .get("tag_name")
                    .and_then(|t| t.as_str())
                    .zip(val.get("tarball_url").and_then(|u| u.as_str()))
                    .map(|(tag, url)| (tag.to_string(), url.to_string())),
                Err(err) => {
                    notes.push(format!("no tagged release read: {}", err));
                    None
                }
            };
        if let Some((tag_name, tarball_url)) = release {
            if probe_source_archive(&tarball_url, probe).is_some() {
                return upstream_row(
                    &repo,
                    repo_url_str,
                    format!("GitHub source release for {}/{}", owner, repo),
                    Some(tag_name.trim_start_matches('v').to_string()),
                    Some(tag_name.clone()),
                    tarball_url,
                    format!("Verified GitHub source archive for tag '{}'", tag_name),
                );
            }
            notes.push(format!(
                "the archive advertised for tag '{}' could not be fetched",
                tag_name
            ));
        }

        // Repositories without releases are still buildable from a branch snapshot.
        let default_branch = match github_api_json(&format!("/repos/{}/{}", owner, repo), probe) {
            Ok(val) => match val.get("default_branch").and_then(|b| b.as_str()) {
                Some(branch) => Some(branch.to_string()),
                None => {
                    notes.push("the repository response named no default branch".to_string());
                    None
                }
            },
            Err(err) => {
                notes.push(format!("repository metadata read: {}", err));
                None
            }
        };
        if let Some(branch) = default_branch {
            let url = format!(
                "https://github.com/{}/{}/archive/refs/heads/{}.tar.gz",
                owner, repo, branch
            );
            if let Some(resolved) = probe_source_archive(&url, probe) {
                return upstream_row(
                    &repo,
                    repo_url_str,
                    format!(
                        "GitHub source snapshot of branch '{}' for {}/{}",
                        branch, owner, repo
                    ),
                    revision_from_archive_url(&resolved),
                    Some(branch.clone()),
                    url,
                    format!("Using the '{}' branch source snapshot", branch),
                );
            }
            notes.push(format!(
                "the snapshot of branch '{}' could not be fetched",
                branch
            ));
        }

        // Unauthenticated API reads are rate limited per egress IP, and a rate
        // limit hides a branch that GitHub's own archive endpoint resolves as HEAD.
        let url = format!("https://github.com/{}/{}/archive/HEAD.tar.gz", owner, repo);
        if let Some(resolved) = probe_source_archive(&url, probe) {
            let revision = revision_from_archive_url(&resolved);
            let shown = revision.clone().unwrap_or_else(|| "HEAD".to_string());
            return upstream_row(
                &repo,
                repo_url_str,
                format!("GitHub source snapshot of {}/{}", owner, repo),
                revision,
                Some("HEAD".to_string()),
                url,
                format!(
                    "Using the HEAD branch snapshot of '{}'; the release and branch listings were unreadable ({})",
                    shown,
                    notes.join("; ")
                ),
            );
        }
        notes.push("no branch snapshot was reachable".to_string());
        return upstream_unavailable(repo_url_str, notes);
    }

    if let Some(path) = parse_gitlab_repo(repo_url_str) {
        let project = path.split('/').next_back().unwrap_or_default().to_string();
        let url = format!(
            "https://gitlab.com/{}/-/archive/HEAD/{}-HEAD.tar.gz",
            path, project
        );
        if let Some(resolved) = probe_source_archive(&url, probe) {
            let revision = revision_from_archive_url(&resolved);
            let shown = revision.clone().unwrap_or_else(|| "HEAD".to_string());
            return upstream_row(
                &project,
                repo_url_str,
                format!("GitLab source snapshot of {}", path),
                revision,
                Some("HEAD".to_string()),
                url,
                format!("Using the HEAD branch snapshot of '{}' ({})", path, shown),
            );
        }
        notes.push(format!(
            "no HEAD branch snapshot was reachable for '{}'",
            path
        ));
        return upstream_unavailable(repo_url_str, notes);
    }

    upstream_unavailable(
        repo_url_str,
        vec![format!(
            "'{}' is not a github.com or gitlab.com repository ArchBridge can resolve",
            repo_url_str
        )],
    )
}

/// One upstream candidate row: the archive URL is what `v1.prepare build` downloads.
fn upstream_row(
    name: &str,
    repo_url: &str,
    description: String,
    version: Option<String>,
    repository: Option<String>,
    url: String,
    reason: String,
) -> DecisionRow {
    DecisionRow {
        source: "upstream".to_string(),
        availability: "available".to_string(),
        recommended: false,
        reason,
        candidates: vec![Candidate {
            name: name.to_string(),
            version,
            url: Some(url),
            source_url: Some(repo_url.to_string()),
            description: Some(description),
            repository,
            ..Default::default()
        }],
    }
}

fn upstream_unavailable(repo_url: &str, notes: Vec<String>) -> DecisionRow {
    DecisionRow {
        source: "upstream".to_string(),
        availability: "unavailable".to_string(),
        recommended: false,
        reason: if notes.is_empty() {
            format!(
                "Could not resolve upstream release source for '{}'",
                repo_url
            )
        } else {
            format!(
                "Could not resolve upstream release source for '{}': {}",
                repo_url,
                notes.join("; ")
            )
        },
        candidates: vec![],
    }
}

/// Read a GitHub API path. Failure text is kept because the reason matters to the
/// caller: `curl -f` discards the body that explains a 403 rate limit.
fn github_api_json(path: &str, probe: &dyn Probe) -> Result<serde_json::Value, String> {
    let api_url = format!("https://api.github.com{}", path);
    let step = curl_step(
        "-sSL",
        &["-H", "User-Agent: archbridge"],
        &api_url,
        "Query GitHub API",
        10,
    );
    let out = probe.query(&step)?;
    if out.exit_code != 0 {
        let detail = out.stderr.trim();
        return Err(if detail.is_empty() {
            format!("request for {} failed", path)
        } else {
            format!("request for {} failed: {}", path, detail)
        });
    }
    let val: serde_json::Value = serde_json::from_str(out.stdout.trim())
        .map_err(|_| format!("{} did not return JSON", path))?;
    if let Some(message) = val.get("message").and_then(|m| m.as_str()) {
        return Err(format!("{} returned '{}'", path, message));
    }
    Ok(val)
}

/// Ask the URL itself whether it serves an archive, returning the address it
/// resolves to. A release listing can name an asset that is gone, and a 404 page
/// would otherwise reach the archive parser.
fn probe_source_archive(url: &str, probe: &dyn Probe) -> Option<String> {
    let step = curl_step(
        "-sSIL",
        &[
            "-o",
            "/dev/null",
            "-w",
            "%{http_code} %{content_type} %{url_effective}",
        ],
        url,
        "Verify upstream source archive",
        15,
    );
    match probe.query(&step) {
        Ok(out) if out.exit_code == 0 => match out.stdout.lines().last() {
            Some(final_response) => {
                let mut fields = final_response.split_whitespace();
                let is_archive = fields.next() == Some("200")
                    && !fields.next().unwrap_or("text/html").starts_with("text/");
                if is_archive {
                    // The address curl ended up at is what a build would fetch.
                    // A redirect that left https means the bytes are not the ones
                    // the release page described, and a plain-HTTP answer can be
                    // replaced by anyone on the path, so an effective URL that is
                    // not fetchable over TLS disqualifies the archive instead of
                    // falling back to the address it was redirected away from.
                    match fields.next() {
                        Some(final_url) => sanitize_url(Some(final_url))
                            .filter(|resolved| resolved.starts_with("https://")),
                        // Nothing was printed to name the destination, so the
                        // request URL is the only address worth reporting.
                        None => Some(url.to_string()),
                    }
                } else {
                    None
                }
            }
            None => Some(url.to_string()),
        },
        _ => None,
    }
}

/// A pinned snapshot URL ends in the commit it contains, which is the only
/// version information a branch archive carries.
fn revision_from_archive_url(url: &str) -> Option<String> {
    let last = url.rsplit('/').next().filter(|s| !s.is_empty())?;
    (last.len() >= 7 && last.len() <= 40 && last.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| last.to_string())
}

/// The `group/project` path of a GitLab repository URL. Subgroups stay in the
/// path because that is how GitLab addresses projects; extra segments such as
/// `/-/tree/main` are rejected rather than folded into an archive URL.
pub fn parse_gitlab_repo(url: &str) -> Option<String> {
    let trimmed = url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let after_scheme = match trimmed.split_once("://") {
        // Same rule as GitHub: a project address names where the bytes come
        // from, and a plain-HTTP or credential-bearing one is not an address
        // this engine will fetch from.
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some((_, _)) => return None,
        None => trimmed,
    };
    let segments: Vec<&str> = after_scheme.split('/').filter(|s| !s.is_empty()).collect();
    if segments.first().map(|h| h.to_lowercase()) != Some("gitlab.com".to_string()) {
        return None;
    }
    let path = &segments[1..];
    if path.len() < 2
        || path
            .iter()
            .any(|segment| *segment == "-" || segment.contains(".git") || !is_package_name(segment))
    {
        return None;
    }
    Some(path.join("/"))
}

fn query_github_repositories(target: &str, probe: &dyn Probe) -> DecisionRow {
    let query = target
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || c.is_ascii_whitespace())
        .collect::<String>()
        .split_whitespace()
        .map(encode_url_segment)
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
    let step = curl_step(
        "-fsSL",
        &["-H", "User-Agent: archbridge"],
        &api_url,
        "Search GitHub repositories",
        10,
    );
    let output = match probe.query(&step) {
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
    if normalized_target.is_empty() {
        return DecisionRow {
            source: "upstream".to_string(),
            availability: "unavailable".to_string(),
            recommended: false,
            reason: "GitHub search requires a package name or repository URL".to_string(),
            candidates: vec![],
        };
    }
    let mut candidates: Vec<Candidate> = value
        .get("items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            let normalized_name = name.to_ascii_lowercase().replace(['-', '_', ' '], "");
            // An empty name matches everything, which would list the whole result
            // set as if the user had asked for it.
            if normalized_name.is_empty() {
                return None;
            }
            if !normalized_name.contains(&normalized_target)
                && !normalized_target.contains(&normalized_name)
            {
                return None;
            }
            // The archive URL is built from these two segments, so they must be a
            // plain `owner/repo` pair and nothing else.
            let (owner, repo) = parse_github_repo(item.get("full_name")?.as_str()?)?;
            let branch = item
                .get("default_branch")
                .and_then(|branch| branch.as_str())
                .filter(|branch| is_package_name(branch))
                .unwrap_or("main");
            let html_url = sanitize_url(item.get("html_url").and_then(|url| url.as_str()))?;
            Some(Candidate {
                name: format!("{owner}/{repo}"),
                version: None,
                url: Some(format!(
                    "https://github.com/{}/{}/archive/refs/heads/{}.tar.gz",
                    owner, repo, branch
                )),
                source_url: Some(html_url),
                description: sanitize_opt(item.get("description").and_then(|d| d.as_str())),
                ..Default::default()
            })
        })
        .take(MAX_CANDIDATES)
        .collect();
    rank_candidates(&mut candidates, target);

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

/// Extract `(owner, repo)` from a GitHub URL. The host must be the whole first
/// path-bearing segment: matching `github.com` anywhere in the string would
/// happily accept `https://notgithub.com.example/owner/repo`.
pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    // A bare `owner/repo` (as GitHub search reports `full_name`) is accepted, and
    // anything else must be a github.com URL whose query/fragment says nothing
    // about which repository is meant.
    let trimmed = url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let (host_part, path_part) = match trimmed.split_once("://") {
        Some((scheme, rest)) => {
            // A repository address is where a build's bytes come from, and a
            // plain-HTTP one can be answered by anyone. Credentials in the URL
            // are refused by the host check below, which compares the whole
            // authority against a fixed name.
            if !scheme.eq_ignore_ascii_case("https") {
                return None;
            }
            let mut parts = rest.splitn(2, '/');
            (
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
            )
        }
        // No scheme: either a bare `owner/repo` (how GitHub search reports
        // `full_name`) or a host-first URL such as `github.com/owner/repo`.
        None => {
            if trimmed.contains('/') {
                let mut parts = trimmed.splitn(2, '/');
                let first = parts.next().unwrap_or_default();
                let rest = parts.next().unwrap_or_default();
                if first.contains('.') {
                    (first, rest)
                } else {
                    ("github.com", trimmed)
                }
            } else {
                return None;
            }
        }
    };
    if !matches!(
        host_part.to_ascii_lowercase().as_str(),
        "github.com" | "www.github.com"
    ) {
        return None;
    }
    let mut segments = path_part.split('/').filter(|segment| !segment.is_empty());
    let owner = segments.next()?;
    let repo = segments.next()?;
    if !is_package_name(owner) || !is_package_name(repo) {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repo_requires_exact_host_segment() {
        assert_eq!(
            parse_github_repo("https://github.com/torvalds/linux"),
            Some(("torvalds".to_string(), "linux".to_string()))
        );
        assert_eq!(
            parse_github_repo("github.com/torvalds/linux.git/"),
            Some(("torvalds".to_string(), "linux".to_string()))
        );
        // A substring match let an attacker-controlled host through.
        assert_eq!(
            parse_github_repo("https://notgithub.com.example/torvalds/linux"),
            None
        );
        assert_eq!(
            parse_github_repo("https://evil.example/redirect?u=github.com/torvalds/linux"),
            None
        );
        assert_eq!(parse_github_repo("https://github.com/torvalds"), None);
    }

    #[test]
    fn gitlab_repo_requires_the_exact_host_and_a_project_path() {
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/gitlab-org/cli").as_deref(),
            Some("gitlab-org/cli")
        );
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/group/sub/project.git/").as_deref(),
            Some("group/sub/project")
        );
        // A URL naming a path inside the project is not the project itself.
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/gitlab-org/cli/-/archive/HEAD/cli-HEAD.tar.gz"),
            None
        );
        assert_eq!(parse_gitlab_repo("https://notgitlab.com.example/a/b"), None);
        assert_eq!(parse_gitlab_repo("https://gitlab.com/single"), None);
        assert_eq!(
            parse_gitlab_repo("https://evil.example/redirect?u=gitlab.com/a/b"),
            None
        );
    }

    #[test]
    fn only_a_commit_shaped_final_segment_counts_as_a_revision() {
        assert_eq!(
            revision_from_archive_url(
                "https://codeload.github.com/o/r/tar.gz/3fce3b5bb0236da2df6d99672afb8a719642eca7"
            )
            .as_deref(),
            Some("3fce3b5bb0236da2df6d99672afb8a719642eca7")
        );
        assert_eq!(
            revision_from_archive_url("https://github.com/o/r/archive/HEAD.tar.gz"),
            None
        );
        assert_eq!(
            revision_from_archive_url("https://gitlab.com/g/p/-/archive/HEAD/p-HEAD.tar.gz"),
            None
        );
    }

    #[test]
    fn url_encoding_cannot_shift_the_requested_resource() {
        assert_eq!(encode_url_segment("hello-world_1.2~3"), "hello-world_1.2~3");
        assert_eq!(encode_url_segment("a b"), "a%20b");
        // Each of these would otherwise terminate the path segment or add a query.
        let encoded = encode_url_segment("../../rpc?v=1#x&y=()");
        assert!(!encoded.contains(' '));
        assert!(!encoded.contains('?'));
        assert!(!encoded.contains('#'));
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('/'));
        assert_eq!(encoded, "..%2F..%2Frpc%3Fv%3D1%23x%26y%3D%28%29");
    }

    /// `1.10` is newer than `1.9` and `1:0.1` is newer than `99.0`: text order
    /// says the opposite for both, and ranking by text order recommends the older
    /// release.
    #[test]
    fn versions_order_the_way_alpm_does() {
        for (older, newer) in [
            ("1.9", "1.10"),
            ("1.0", "1.0.1"),
            ("2.0-1", "2.0-2"),
            ("99.0", "1:0.1"),
            ("0:1.0", "1:0.1"),
            ("1.0~rc1", "1.0"),
            ("1.0~rc1", "1.0~rc2"),
            ("v1.0", "1.0"),
            ("1.9.5", "1.10.0"),
            ("1.0.beta", "1.0-2"),
            ("9", "10"),
            ("1.0.0", "1.0.0.1"),
            ("", "1"),
        ] {
            assert_eq!(
                compare_versions(older, newer),
                Ordering::Less,
                "{older} should sort below {newer}"
            );
            assert_eq!(
                compare_versions(newer, older),
                Ordering::Greater,
                "{newer} should sort above {older}"
            );
        }
        for (same, other) in [
            ("1.10-1", "1.10-1"),
            ("1:2.0", "1:2.0"),
            ("1.0", "1.0-"),
            ("1_0", "1.0"),
            ("01.02", "1.2"),
        ] {
            assert_eq!(
                compare_versions(same, other),
                Ordering::Equal,
                "{same} and {other} are the same version"
            );
        }
        // Hostile shapes must be ordered, not panicked on or wrapped.
        assert_ne!(
            compare_versions(&"9".repeat(400), &"1".repeat(401)),
            Ordering::Equal
        );
        assert_eq!(compare_versions("", ""), Ordering::Equal);
        assert_eq!(compare_versions(":::", "==="), Ordering::Equal);
        // Multi-byte text over the chunking ceiling must be cut at a character
        // boundary, or the ordering itself becomes the panic.
        for hostile in [&"…".repeat(200), &"🐍".repeat(200), &"é".repeat(300)] {
            let _ = compare_versions(hostile, "1.0");
            let _ = compare_versions("1.0", hostile);
        }
    }

    #[test]
    fn a_duplicate_record_keeps_the_newest_version_only() {
        let mut candidates = vec![
            Candidate {
                name: "htop".to_string(),
                version: Some("1.9-1".to_string()),
                repository: Some("extra".to_string()),
                ..Default::default()
            },
            Candidate {
                name: "htop".to_string(),
                version: Some("1.10-1".to_string()),
                repository: Some("extra".to_string()),
                ..Default::default()
            },
            Candidate {
                name: "HTOP".to_string(),
                version: None,
                repository: Some("extra".to_string()),
                ..Default::default()
            },
            Candidate {
                name: "htop".to_string(),
                version: Some("0.1".to_string()),
                repository: Some("testing".to_string()),
                ..Default::default()
            },
        ];
        dedupe_candidates(&mut candidates);
        assert_eq!(candidates.len(), 2, "{candidates:?}");
        assert_eq!(candidates[0].version.as_deref(), Some("1.10-1"));
        assert_eq!(candidates[0].name, "htop");
        // A different repository is a different choice, not a duplicate.
        assert_eq!(candidates[1].repository.as_deref(), Some("testing"));
    }

    #[test]
    fn name_matching_folds_case_the_way_the_databases_do() {
        assert!(name_matches("MyHtop", "htop"));
        assert!(name_matches("htop", "HTOP"));
        assert!(name_matches("htop-qt", "htop"));
        assert!(!name_matches("vim", "htop"));
    }

    #[test]
    fn an_asset_name_states_the_machine_it_runs_on() {
        assert_eq!(
            asset_architecture("Tool-1.0-x86_64.AppImage"),
            Some("x86_64")
        );
        assert_eq!(asset_architecture("tool.amd64.flatpak"), Some("x86_64"));
        assert_eq!(asset_architecture("tool-aarch64.AppImage"), Some("aarch64"));
        assert_eq!(asset_architecture("Tool-ARM64.app"), Some("aarch64"));
        assert_eq!(asset_architecture("tool-i386.deb"), Some("i686"));
        assert_eq!(asset_architecture("tool-armhf.bin"), Some("armv7"));
        // Saying nothing is not the same as saying the wrong thing.
        assert_eq!(asset_architecture("tool.AppImage"), None);
        assert_eq!(asset_architecture("charmel-1.0.AppImage"), None);
        assert_eq!(asset_architecture("x86_64-suse-toolbox"), Some("x86_64"));
        assert!(ends_extension("Tool.APPIMAGE", ".appimage"));
        assert!(ends_extension("tool.Flatpakref", ".flatpakref"));
        assert!(!ends_extension("tool.AppImage.sig", ".appimage"));
    }

    #[test]
    fn a_repo_address_must_be_https_and_free_of_credentials() {
        // The bare and host-first forms carry no transport to negotiate.
        assert_eq!(
            parse_github_repo("owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );
        assert_eq!(
            parse_github_repo("https://github.com/owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );
        assert_eq!(parse_github_repo("http://github.com/owner/repo"), None);
        assert_eq!(parse_github_repo("HTTP://github.com/owner/repo"), None);
        assert_eq!(parse_github_repo("https://user:pass@github.com/a/b"), None);
        assert_eq!(parse_gitlab_repo("http://gitlab.com/o/r"), None);
        assert_eq!(parse_gitlab_repo("ssh://git@gitlab.com/o/r"), None);
        assert_eq!(
            parse_gitlab_repo("https://gitlab.com/o/r"),
            Some("o/r".to_string())
        );
    }
}
