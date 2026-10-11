//! Discovery sources driven through the probe seam, entirely offline: canned
//! pacman/AUR/GitHub payloads in, `DecisionReport` out. Every case here covers a
//! response shape that used to produce a panic, a silently empty name, an
//! un-fetchable URL, or a terminal escape in a review.

use archbridge::config::Config;
use archbridge::discovery::{search_with, Probe};
use archbridge::process::{ProcessOutput, Step};
use serde_json::json;
use std::cell::RefCell;

struct Fake {
    /// (substring of the command line, canned reply) pairs, first match wins.
    replies: Vec<(String, Result<ProcessOutput, String>)>,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            replies: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn on(&mut self, needle: &str, stdout: &str) -> &mut Self {
        self.replies.push((
            needle.to_string(),
            Ok(ProcessOutput {
                exit_code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            }),
        ));
        self
    }

    fn fail(&mut self, needle: &str, message: &str) -> &mut Self {
        self.replies
            .push((needle.to_string(), Err(message.to_string())));
        self
    }

    fn command_lines(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Probe for Fake {
    fn query(&self, step: &Step) -> Result<ProcessOutput, String> {
        let line = format!("{} {}", step.program, step.args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        for (needle, reply) in &self.replies {
            if line.contains(needle.as_str()) {
                return reply.clone();
            }
        }
        Err(format!("no canned reply for: {line}"))
    }
}

fn all_enabled() -> Config {
    Config {
        path: std::path::PathBuf::from("/nonexistent/archbridge-config.json"),
        ..Config::default()
    }
}

fn row<'a>(
    report: &'a archbridge::discovery::DecisionReport,
    source: &str,
) -> &'a archbridge::discovery::DecisionRow {
    report
        .decision_table
        .iter()
        .find(|row| row.source == source)
        .unwrap_or_else(|| panic!("no '{source}' row in {:?}", report.decision_table))
}

#[test]
fn a_blank_or_unqueryable_target_never_starts_a_discovery() {
    for target in [
        "",
        "   ",
        "\t\n",
        &"h".repeat(5000),
        "has\0nul",
        "line\nbreak",
    ] {
        let fake = Fake::new();
        let report = search_with(target, None, &all_enabled(), &fake);
        assert!(
            fake.command_lines().is_empty(),
            "'{target}' still ran queries: {:?}",
            fake.command_lines()
        );
        assert_eq!(report.recommended_source, None, "{target}");
        for row in &report.decision_table {
            assert_eq!(row.availability, "unavailable", "{target}: {:?}", row);
            assert!(row.candidates.is_empty(), "{target}: {:?}", row);
        }
    }
}

#[test]
fn a_target_is_percent_encoded_into_the_aur_request() {
    let mut fake = Fake::new();
    fake.on("aur.archlinux.org", "{}");
    search_with("../../rpc?v=1#x", None, &all_enabled(), &fake);
    let lines = fake.command_lines();
    let aur = lines
        .iter()
        .find(|line| line.contains("aur.archlinux.org"))
        .expect("the AUR query ran");
    // The slashes, query and fragment are gone, so the request still addresses
    // the search path and nothing else.
    assert!(
        !aur.contains("/rpc/v5/search/../"),
        "traversal reached the URL: {aur}"
    );
    assert!(
        !aur.contains('?') && !aur.contains('#'),
        "a query or fragment reached the URL: {aur}"
    );
    assert!(aur.contains("%2F") && aur.contains("%3F"), "{aur}");
}

#[test]
fn disabled_sources_are_never_queried() {
    let mut config = all_enabled();
    config.official = false;
    config.aur = false;
    let mut fake = Fake::new();
    fake.on("curl", r#"{"results":[]}"#);
    let report = search_with("htop", None, &config, &fake);
    let lines = fake.command_lines();
    assert!(
        !lines.iter().any(|line| line.contains("pacman")),
        "a disabled source was queried: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("aur.archlinux.org")),
        "a disabled source was queried: {lines:?}"
    );
    assert_eq!(row(&report, "official").availability, "disabled");
    assert_eq!(row(&report, "aur").availability, "disabled");
    assert_eq!(row(&report, "flatpak").availability, "unavailable");
}

#[test]
fn pacman_records_are_parsed_pairwise_and_ranked() {
    let canned = "\
extra/htop 3.3.0-1
Interactive process viewer
core/util-linux 2.40-1
Utilities
extra/htop-clang 1.0-1
Built by clang
";
    let mut fake = Fake::new();
    fake.on("pacman -Ss", canned);
    fake.on("pacman -Si", "");
    let report = search_with("htop", None, &all_enabled(), &fake);
    let official = row(&report, "official");
    assert_eq!(official.availability, "available", "{official:?}");
    let names: Vec<&str> = official
        .candidates
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect();
    assert_eq!(names, vec!["htop", "htop-clang"], "{names:?}");
    assert_eq!(
        official.candidates[0].description.as_deref(),
        Some("Interactive process viewer")
    );
    assert_eq!(
        official.candidates[0].repository.as_deref(),
        Some("extra"),
        "the repository is the segment before the slash"
    );
    assert_eq!(official.candidates[0].version.as_deref(), Some("3.3.0-1"));
}

#[test]
fn a_description_that_is_really_the_next_record_is_not_used() {
    // Truncated output: the "description" line is the following package header.
    let canned = "\
extra/one 1.0-1
extra/two 2.0-1
the real description of two
";
    let mut fake = Fake::new();
    fake.on("pacman -Ss", canned);
    fake.on("pacman -Si", "");
    let report = search_with("one", None, &all_enabled(), &fake);
    let candidates = &row(&report, "official").candidates;
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(candidates[0].description, None, "the next record leaked in");
}

#[test]
fn malformed_pacman_output_is_survivable() {
    for canned in [
        "",
        "\n\n\n",
        "no-separator-line\n",
        "extra/\n",
        "/orphan 1.0-1\n",
        "extra/name\n", // header with no version at all
        "extra/name \n",
        "extra/na me 1.0-1\n", // a name with a space is not a package name
        "extra/$(id) 1.0-1\n",
        "extra/../etc 1.0-1\n",
        &format!("extra/{} 1.0-1\nlong\n", "x".repeat(300)),
        "extra/name 1.0-1\x1b[31mESC\x1b[0m\n",
    ] {
        let mut fake = Fake::new();
        fake.on("pacman -Ss", canned);
        fake.on("pacman -Si", "");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            search_with("name", None, &all_enabled(), &fake)
        }));
        assert!(outcome.is_ok(), "panic on pacman output {canned:?}");
        let report = search_with("name", None, &all_enabled(), &fake);
        let official = row(&report, "official");
        for candidate in &official.candidates {
            assert!(
                !candidate.name.is_empty(),
                "an empty name was listed for {canned:?}"
            );
            assert!(
                !candidate.name.contains('\u{1b}'),
                "an escape sequence reached the review: {:?}",
                candidate.name
            );
            assert!(
                candidate.name.len() <= 128,
                "an unbounded name was listed: {}",
                candidate.name.len()
            );
            if let Some(description) = &candidate.description {
                assert!(
                    !description.contains('\u{1b}'),
                    "an escape sequence reached the review: {description}"
                );
            }
        }
    }
}

#[test]
fn official_metadata_from_si_is_used_and_cleaned() {
    let si = "\
Name            : htop
Repository      : extra
Version         : 3.3.0-1
Description     : Interactive process viewer
                    continued here
URL             : javascript:alert(1)
Licenses        : MIT
Maintainer      : Unknown Packager
Packager        : Arch Linux Team
Architecture    : x86_64
Installed Size  : 241.00 KiB
";
    let mut fake = Fake::new();
    fake.on("pacman -Ss", "extra/htop 3.3.0-1\n");
    fake.on("pacman -Si", si);
    let report = search_with("htop", None, &all_enabled(), &fake);
    let candidate = &row(&report, "official").candidates[0];
    assert_eq!(candidate.repository.as_deref(), Some("extra"));
    assert_eq!(candidate.version.as_deref(), Some("3.3.0-1"));
    assert_eq!(
        candidate.description.as_deref(),
        Some("Interactive process viewer continued here"),
        "the header line carries no description, so -Si fills it in, wrapped lines rejoined"
    );
    assert_eq!(candidate.license.as_deref(), Some("MIT"));
    assert_eq!(candidate.architecture.as_deref(), Some("x86_64"));
    assert_eq!(candidate.size.as_deref(), Some("241.00 KiB"));
    assert_eq!(candidate.maintainer.as_deref(), Some("Arch Linux Team"));
    assert_eq!(
        candidate.url, None,
        "a non-http URL from a database must not be offered"
    );
}

#[test]
fn aur_results_are_validated_field_by_field() {
    let body = json!({
        "resultcount": 5,
        "results": [
            {"Name": "htop", "Version": "3.3.0-1", "Description": "viewer",
             "URL": "https://htop.dev", "URLPath": "/cgit/aur.git/snapshot/htop.tar.gz",
             "Maintainer": "someone", "OutOfDate": "2024-01-01"},
            {"Name": "", "Version": "1"},
            {"Name": "has space", "Version": "1"},
            {"Name": "$(reboot)", "Version": "1"},
            {"Name": "htop-other", "URLPath": "//evil.example/htop.tar.gz"},
            {"Name": "unrelated-pkg", "Version": "1"},
            {"Version": "no name at all"},
            {"Name": "null-fields", "Description": null, "URL": 42, "OutOfDate": 1700000000}
        ]
    })
    .to_string();

    let mut fake = Fake::new();
    fake.on("aur.archlinux.org", &body);
    let report = search_with("htop", None, &all_enabled(), &fake);
    let aur = row(&report, "aur");
    assert_eq!(aur.availability, "available", "{aur:?}");
    let names: Vec<&str> = aur.candidates.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["htop", "htop-other"], "{names:?}");
    assert_eq!(
        aur.candidates[0].source_url.as_deref(),
        Some("https://aur.archlinux.org/cgit/aur.git/snapshot/htop.tar.gz")
    );
    assert_eq!(aur.candidates[0].out_of_date.as_deref(), Some("2024-01-01"));
    // A URLPath that would change the authority is dropped, not rewritten.
    assert_eq!(
        aur.candidates[1].source_url, None,
        "a '//' path escaped the AUR host"
    );
    // Wrongly typed fields become absent rather than "null" or a number.
    assert_eq!(aur.candidates[0].description.as_deref(), Some("viewer"));
}

#[test]
fn a_broken_aur_response_reports_unavailable_without_panicking() {
    for body in [
        "",
        "   ",
        "<html>502 bad gateway</html>",
        "{",
        r#"{"results": {}}"#,
        r#"{"results": "nope"}"#,
        "[]",
        r#"{"resultcount":0}"#,
    ] {
        let mut fake = Fake::new();
        fake.on("aur.archlinux.org", body);
        let report = search_with("htop", None, &all_enabled(), &fake);
        let aur = row(&report, "aur");
        assert!(
            matches!(aur.availability.as_str(), "unavailable" | "unknown"),
            "'{body}' produced {:?}",
            aur.availability
        );
        assert!(aur.candidates.is_empty(), "'{body}' produced {aur:?}");
    }

    // A transport failure is "unknown", which is not the same as "absent".
    let mut fake = Fake::new();
    fake.fail("curl", "Failed to spawn process 'curl': not found");
    let report = search_with("htop", None, &all_enabled(), &fake);
    assert_eq!(row(&report, "aur").availability, "unknown");
}

#[test]
fn aur_candidates_are_capped_at_a_readable_number() {
    let results: Vec<_> = (0..500)
        .map(|index| json!({"Name": format!("htop-tool-{index}"), "Version": "1"}))
        .collect();
    let body = json!({"results": results}).to_string();
    let mut fake = Fake::new();
    fake.on("aur.archlinux.org", &body);
    let report = search_with("htop", None, &all_enabled(), &fake);
    let aur = row(&report, "aur");
    assert!(
        aur.candidates.len() <= 25,
        "{} candidates is not a review",
        aur.candidates.len()
    );
}

#[test]
fn release_assets_need_a_downloadable_https_location() {
    let body = json!({
        "tag_name": "v1.2.3",
        "assets": [
            {"name": "Tool-x86_64.AppImage", "browser_download_url": "https://example.com/a.AppImage"},
            {"name": "Insecure.AppImage", "browser_download_url": "http://example.com/b.AppImage"},
            {"name": "Missing.AppImage"},
            {"name": "", "browser_download_url": "https://example.com/c.AppImage"},
            {"name": "Escapes.AppImage", "browser_download_url": "https://example.com/\n../../x"},
            {"name": "tool.flatpakref", "browser_download_url": "https://example.com/t.flatpakref"},
            {"name": "notes.txt", "browser_download_url": "https://example.com/notes.txt"}
        ]
    })
    .to_string();

    let mut fake = Fake::new();
    fake.on("api.github.com/repos/o/r/releases/latest", &body);
    let report = search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    let appimage = row(&report, "appimage");
    assert_eq!(appimage.availability, "available", "{appimage:?}");
    let names: Vec<&str> = appimage
        .candidates
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect();
    assert_eq!(names, vec!["Tool-x86_64.AppImage"], "{names:?}");
    assert_eq!(
        appimage.candidates[0].url.as_deref(),
        Some("https://example.com/a.AppImage")
    );
    assert_eq!(appimage.candidates[0].version.as_deref(), Some("v1.2.3"));

    let flatpak = row(&report, "flatpak");
    assert_eq!(
        flatpak.candidates.len(),
        1,
        "only the flatpakref belongs to the flatpak row: {flatpak:?}"
    );
}

#[test]
fn a_release_host_that_is_not_github_is_explained_not_queried() {
    let mut fake = Fake::new();
    fake.on("curl", "{}");
    let report = search_with(
        "tool",
        Some("https://notgithub.com.example/o/r"),
        &all_enabled(),
        &fake,
    );
    assert_eq!(row(&report, "appimage").availability, "unavailable");
    assert!(row(&report, "appimage").reason.contains("not a recognized"));

    // Traversal in the override never reaches the request builder.
    let report = search_with(
        "tool",
        Some("https://github.com/../../evil"),
        &all_enabled(),
        &fake,
    );
    assert_eq!(row(&report, "appimage").availability, "unavailable");
    let lines = fake.command_lines();
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("api.github.com/repos/../")),
        "{lines:?}"
    );
}

#[test]
fn upstream_prefers_a_tagged_release_and_falls_back_with_notes() {
    let release =
        json!({"tag_name": "1.2.3", "tarball_url": "https://codeload.github.com/o/r/tar.gz/1.2.3"})
            .to_string();
    let mut fake = Fake::new();
    fake.on("/repos/o/r/releases/latest", &release);
    fake.on(
        "-sSIL",
        "200 application/gzip https://codeload.github.com/o/r/tar.gz/1.2.3",
    );
    let report = search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    let upstream = row(&report, "upstream");
    assert_eq!(upstream.availability, "available", "{upstream:?}");
    assert_eq!(upstream.candidates[0].version.as_deref(), Some("1.2.3"));
    assert_eq!(
        upstream.candidates[0].url.as_deref(),
        Some("https://codeload.github.com/o/r/tar.gz/1.2.3")
    );

    // No tagged release: the branch snapshot is offered, and the reason says why.
    let mut fake = Fake::new();
    fake.on("/releases/latest", r#"{"message":"Not Found"}"#);
    fake.on(
        "-sSL",
        json!({"default_branch": "main"}).to_string().as_str(),
    );
    fake.on(
        "-sSIL",
        "200 application/gzip https://codeload.github.com/o/r/tar.gz/refs/heads/main",
    );
    let report = search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    let upstream = row(&report, "upstream");
    assert_eq!(upstream.availability, "available", "{upstream:?}");
    assert!(upstream.reason.contains("main"), "{}", upstream.reason);

    // Nothing reachable is an unavailable row, never a candidate list.
    let mut fake = Fake::new();
    fake.on("/releases/latest", "not json");
    fake.on("-sSL", "not json");
    fake.on(
        "-sSIL",
        "404 text/html https://github.com/o/r/archive/HEAD.tar.gz",
    );
    let report = search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    let upstream = row(&report, "upstream");
    assert_eq!(upstream.availability, "unavailable", "{upstream:?}");
    assert!(upstream.candidates.is_empty());
    assert!(
        upstream.reason.to_lowercase().contains("could not"),
        "{}",
        upstream.reason
    );
}

#[test]
fn a_probe_reply_with_missing_fields_is_not_an_archive() {
    // curl -w prints fewer fields than expected when it is interrupted.
    for reply in ["", "200", "200 ", "200 application/gzip", "999"] {
        let mut fake = Fake::new();
        fake.on("/repos/o/r/releases/latest", "{}");
        fake.on("-sSIL", reply);
        fake.on("-sSL", r#"{"default_branch":"main"}"#);
        let report = search_with(
            "tool",
            Some("https://github.com/o/r"),
            &all_enabled(),
            &fake,
        );
        let upstream = row(&report, "upstream");
        // Either the header line was understood, or the source is unavailable —
        // never a candidate whose URL is a fragment of the curl output.
        if let Some(candidate) = upstream.candidates.first() {
            let url = candidate.url.clone().unwrap_or_default();
            assert!(
                url.starts_with("https://"),
                "reply {reply:?} produced {url}"
            );
            assert!(!url.contains(' '), "reply {reply:?} produced {url}");
        }
    }
}

#[test]
fn github_search_needs_a_plain_owner_repo_pair() {
    let body = json!({
        "items": [
            {"name": "tool", "full_name": "owner/tool", "html_url": "https://github.com/owner/tool",
             "default_branch": "main", "description": "A tool"},
            {"name": "", "full_name": "owner/", "html_url": "https://github.com/owner/"},
            {"name": "tool2", "full_name": "../../etc/passwd", "html_url": "https://github.com/x/y"},
            {"name": "tool3", "full_name": "owner/tool3/extra", "html_url": "https://github.com/o/t"},
            {"name": "tool4", "full_name": "own er/tool4", "html_url": "https://github.com/o/t"},
            {"name": "tool5", "full_name": "owner/tool5", "html_url": "http://evil.example/o/t"},
            {"name": "tool6", "full_name": "owner/tool6", "html_url": "https://github.com/o/t",
             "default_branch": "feature/../main", "description": null}
        ]
    })
    .to_string();

    let mut fake = Fake::new();
    fake.on("api.github.com/search/repositories", &body);
    let report = search_with("tool", None, &all_enabled(), &fake);
    let upstream = row(&report, "upstream");
    for candidate in &upstream.candidates {
        let name = candidate.name.as_str();
        let (owner, repo) = name
            .split_once('/')
            .unwrap_or_else(|| panic!("{name} is not owner/repo"));
        assert!(!owner.is_empty() && !repo.is_empty(), "{name}");
        assert!(!name.contains(".."), "{name} can traverse");
        let url = candidate
            .url
            .clone()
            .expect("a search candidate needs a URL");
        assert!(url.starts_with("https://github.com/"), "{url}");
        assert!(!url.contains(".."), "{url}");
        assert!(
            !url.contains("/feature/"),
            "a branch that escapes the ref name reached the URL: {url}"
        );
    }
    let names: Vec<&str> = upstream
        .candidates
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect();
    assert!(names.contains(&"owner/tool"), "{names:?}");
    assert!(
        !names.iter().any(|name| name.contains("passwd")),
        "{names:?}"
    );
}

#[test]
fn a_local_foreign_file_is_only_offered_for_inspection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let deb = dir.path().join("sample.deb");
    std::fs::write(&deb, b"not really a deb").expect("write");

    let target = deb.to_str().expect("utf-8 path");
    let mut fake = Fake::new();
    fake.fail("pacman", "pacman is unavailable");
    fake.fail("curl", "offline");
    let report = search_with(target, None, &all_enabled(), &fake);
    assert_eq!(row(&report, "deb").availability, "available");
    // DEB is inspect-only, so it is never the recommended install source.
    assert_eq!(report.recommended_source.as_deref(), None, "{report:?}");
    assert!(
        !row(&report, "deb").recommended,
        "a foreign file was recommended"
    );
    assert_eq!(row(&report, "rpm").availability, "unavailable");
}

#[test]
fn the_first_available_source_in_preference_order_wins() {
    let mut fake = Fake::new();
    fake.on("pacman -Ss", "extra/htop 3.3.0-1\nviewer\n");
    fake.on("pacman -Si", "");
    fake.on(
        "aur.archlinux.org",
        r#"{"results":[{"Name":"htop-bin","Version":"1"}]}"#,
    );
    let report = search_with("htop", None, &all_enabled(), &fake);
    assert_eq!(report.recommended_source.as_deref(), Some("official"));
    assert!(row(&report, "official").recommended);
    assert!(
        !row(&report, "aur").recommended,
        "a tie must not skip ahead"
    );

    // With the official repositories off, AUR takes the recommendation.
    let mut config = all_enabled();
    config.official = false;
    let report = search_with("htop", None, &config, &fake);
    assert_eq!(report.recommended_source.as_deref(), Some("aur"));
}

/// A release asset is a prebuilt binary for one machine. An arm64 AppImage
/// offered to an x86_64 host is a download that cannot run, and a name spelled
/// `.APPIMAGE` is the same artifact as `.AppImage`.
#[test]
fn a_release_asset_for_another_architecture_is_not_offered() {
    let (host_alias, foreign_alias) = if cfg!(target_arch = "x86_64") {
        ("x86_64", "aarch64")
    } else if cfg!(target_arch = "aarch64") {
        ("aarch64", "x86_64")
    } else {
        eprintln!("unhandled host architecture; skipping");
        return;
    };
    let body = serde_json::json!({
        "tag_name": "v1.2.3",
        "assets": [
            {"name": format!("Tool-{foreign_alias}.AppImage"),
             "browser_download_url": format!("https://example.com/t-{foreign_alias}.AppImage")},
            {"name": format!("Tool-{host_alias}.APPIMAGE"),
             "browser_download_url": format!("https://example.com/t-{host_alias}.APPIMAGE")},
            {"name": "Tool-universal.AppImage",
             "browser_download_url": "https://example.com/t-universal.AppImage"},
            {"name": format!("Tool-{host_alias}.AppImage.debug"),
             "browser_download_url": "https://example.com/t.AppImage.debug"},
            {"name": "Tool.AppImage.sha256",
             "browser_download_url": "https://example.com/t.AppImage.sha256"}
        ]
    })
    .to_string();

    let mut fake = Fake::new();
    fake.on("api.github.com/repos/o/r/releases/latest", &body);
    let report = search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    let appimage = row(&report, "appimage");
    let names: Vec<&str> = appimage
        .candidates
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect();
    assert!(
        !names.iter().any(|name| name.contains(foreign_alias)),
        "an asset built for {foreign_alias} was offered to a {host_alias} host: {names:?}"
    );
    assert!(
        names.contains(&format!("Tool-{host_alias}.APPIMAGE").as_str()),
        "a differently cased extension lost the artifact: {names:?}"
    );
    assert_eq!(names.len(), 2, "{names:?}");
    let kept = &appimage.candidates[0];
    assert_eq!(
        kept.architecture.as_deref(),
        Some(host_alias),
        "the architecture a name states has to reach the review"
    );
    // The asset that names no architecture is kept: silence is not a mismatch.
    assert_eq!(appimage.candidates[1].architecture, None, "{names:?}");
}

/// Both the sync database and the AUR answer a search case-insensitively, so a
/// case-sensitive substring test loses packages the user can plainly install.
#[test]
fn a_source_that_matches_case_insensitively_is_read_case_insensitively() {
    let mut fake = Fake::new();
    fake.on("pacman -Ss", "extra/MyHtopViewer 1.0-1\nviewer\n");
    fake.on("pacman -Si", "");
    let report = search_with("htop", None, &all_enabled(), &fake);
    assert_eq!(
        row(&report, "official").availability,
        "available",
        "an embedded match with different case was dropped: {:?}",
        row(&report, "official")
    );

    let mut fake = Fake::new();
    fake.on(
        "aur.archlinux.org",
        r#"{"results":[{"Name":"MyHtopViewer","Version":"1.0-1"}]}"#,
    );
    let report = search_with("htop", None, &all_enabled(), &fake);
    assert_eq!(
        row(&report, "aur").availability,
        "available",
        "AUR matched case-sensitively: {:?}",
        row(&report, "aur")
    );
}

/// One source can report the same package twice, and the review has to show the
/// newest of the two — `1.10` is newer than `1.9`, which text order gets wrong.
#[test]
fn a_repeated_record_keeps_the_newest_version() {
    let canned = "\
extra/htop 1.9-1
old description
extra/htop 1.10-1
Interactive process viewer
core/htop 0.1-1
a different repository is a different choice
";
    let mut fake = Fake::new();
    fake.on("pacman -Ss", canned);
    fake.on("pacman -Si", "");
    let report = search_with("htop", None, &all_enabled(), &fake);
    let official = row(&report, "official");
    let listed: Vec<(&str, &str)> = official
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.name.as_str(),
                candidate.version.as_deref().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![("htop", "1.10-1"), ("htop", "0.1-1")],
        "the duplicate must collapse to the newer record: {listed:?}"
    );
    assert_eq!(official.candidates[0].repository.as_deref(), Some("extra"));
}

/// A release listing can point at an archive that is served over plain HTTP
/// after a redirect. The bytes then have no integrity at all, so the answer is
/// "unavailable", not a candidate whose download can be replaced in transit.
#[test]
fn an_archive_that_redirects_off_https_is_refused() {
    for reply in [
        "200 application/gzip http://mirror.example/o/r.tar.gz",
        "200 application/octet-stream ftp://mirror.example/o/r.tar.gz",
    ] {
        let mut fake = Fake::new();
        fake.on("/repos/o/r/releases/latest", r#"{"message":"Not Found"}"#);
        fake.on("-sSL", r#"{"default_branch":"main"}"#);
        fake.on("-sSIL", reply);
        let report = search_with(
            "tool",
            Some("https://github.com/o/r"),
            &all_enabled(),
            &fake,
        );
        let upstream = row(&report, "upstream");
        for candidate in &upstream.candidates {
            let url = candidate.url.clone().unwrap_or_default();
            assert!(url.starts_with("https://"), "reply {reply:?} offered {url}");
        }
        assert_eq!(
            upstream.availability, "unavailable",
            "reply {reply:?} was treated as a source: {upstream:?}"
        );
    }

    // A probe that names no final URL still falls back to the https request.
    let mut fake = Fake::new();
    fake.on("/repos/o/r/releases/latest", r#"{"message":"Not Found"}"#);
    fake.on("-sSL", r#"{"default_branch":"main"}"#);
    fake.on("-sSIL", "200 application/gzip");
    let report = search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    assert_eq!(row(&report, "upstream").availability, "available");
}

/// Every request a search makes is a child `curl` that follows redirects: a
/// response may not be a bottomless stream and a redirect may not leave TLS.
#[test]
fn every_curl_request_bounds_its_response_and_its_redirects() {
    let mut fake = Fake::new();
    fake.on("aur.archlinux.org", r#"{"results":[]}"#);
    fake.on("api.github.com", r#"{"assets":[]}"#);
    fake.on(
        "-sSIL",
        "200 application/gzip https://codeload.github.com/o/r/tar.gz/1234567",
    );
    search_with(
        "tool",
        Some("https://github.com/o/r"),
        &all_enabled(),
        &fake,
    );
    let lines = fake.command_lines();
    let curls: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("curl"))
        .collect();
    assert!(curls.len() >= 4, "expected several requests, got {curls:?}");
    for line in curls {
        assert!(
            line.contains("--proto-redir =https"),
            "a redirect could leave TLS: {line}"
        );
        assert!(
            line.contains("--max-filesize"),
            "an unbounded reply: {line}"
        );
    }
}

/// A foreign file is offered for inspection only when it is a file, and the
/// importer that reads it decides by lower-cased suffix.
#[test]
fn a_foreign_file_is_detected_the_way_the_inspector_reads_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let upper = dir.path().join("Sample.DEB");
    std::fs::write(&upper, b"not really a deb").expect("write");
    let directory = dir.path().join("looks-like.deb");
    std::fs::create_dir(&directory).expect("mkdir");

    let mut fake = Fake::new();
    fake.fail("pacman", "pacman is unavailable");
    fake.fail("curl", "offline");
    let report = search_with(upper.to_str().unwrap(), None, &all_enabled(), &fake);
    assert_eq!(
        row(&report, "deb").availability,
        "available",
        "an uppercase suffix is the same format to the inspector"
    );

    let report = search_with(directory.to_str().unwrap(), None, &all_enabled(), &fake);
    assert_eq!(
        row(&report, "deb").availability,
        "unavailable",
        "a directory called x.deb holds no package to inspect"
    );
}

/// Deterministic sweep: hostile targets and hostile payloads through every
/// source at once. Nothing may panic, and no control character or traversal
/// segment may come out in a candidate or a reason.
#[test]
fn fuzz_targets_and_payloads_never_panic_or_leak() {
    let targets = [
        "htop",
        "",
        " ",
        "..",
        "../../etc/passwd",
        "a b",
        "$(id)",
        "`id`",
        "-rf",
        "--help",
        "\u{1b}[31mred\u{1b}[0m",
        "\u{1f600}",
        &"h".repeat(300),
        "http://evil.example/x",
        "https://github.com/o/r",
        "C:\\windows\\win.ini",
        "null\u{0}",
    ];
    let payloads = [
        "",
        "{}",
        "[]",
        "null",
        "not json at all",
        r#"{"results":[]}"#,
        r#"{"results":[{"Name":null,"Version":[]}]}"#,
        r#"{"results":[{"Name":"htop","URLPath":".."}]}"#,
        r#"{"assets":[{"name":"a.AppImage","browser_download_url":"https://x/y.AppImage"}]}"#,
        r#"{"items":[{"name":"htop","full_name":"o/htop","html_url":"https://github.com/o/htop"}]}"#,
        "200 application/gzip https://codeload.github.com/o/r/tar.gz/abc1234",
        "\u{1b}[31mextra\u{1b}[0m/htop 1.0-1\n\u{1b}mdesc\u{1b}[0m",
        "core/htop 1.0-1\nDescription\n",
        "<html><script>alert(1)</script></html>",
        r#"{"message":"API rate limit exceeded"}"#,
    ];

    for target in targets {
        for payload in payloads {
            let mut fake = Fake::new();
            fake.on("pacman -Ss", payload);
            fake.on("pacman -Si", payload);
            fake.on("curl", payload);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                search_with(
                    target,
                    Some("https://github.com/o/r"),
                    &all_enabled(),
                    &fake,
                )
            }));
            let report = outcome
                .unwrap_or_else(|_| panic!("panic for target {target:?} payload {payload:?}"));

            for row in &report.decision_table {
                assert!(
                    !row.reason.contains('\u{1b}'),
                    "escape in reason: {:?}",
                    row.reason
                );
                assert!(
                    !row.reason.contains('\0'),
                    "NUL in reason: {:?}",
                    row.reason
                );
                for candidate in &row.candidates {
                    assert!(!candidate.name.is_empty(), "{:?}", candidate.name);
                    assert!(
                        !candidate.name.contains('\u{1b}'),
                        "escape in name: {:?}",
                        candidate.name
                    );
                    if let Some(url) = &candidate.url {
                        assert!(
                            url.starts_with("http://") || url.starts_with("https://"),
                            "candidate offers {url}"
                        );
                        assert!(!url.contains('\n') && !url.contains(' '), "{url}");
                    }
                    if let Some(description) = &candidate.description {
                        assert!(description.len() < 600, "unbounded description");
                    }
                }
            }
        }
    }
}
