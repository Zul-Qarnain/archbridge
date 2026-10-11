//! Preferences store: atomic save, actionable corruption errors, and the value
//! shapes a repo override / output directory may legally hold.

use archbridge::config::Config;
use std::path::Path;

fn temp_config() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("archbridge").join("config.json");
    (dir, path)
}

#[test]
fn a_saved_config_round_trips_through_a_renamed_temp_file() {
    let (dir, path) = temp_config();
    let mut config = Config {
        path: path.clone(),
        ..Config::default()
    };
    config.aur = false;
    config.output_dir = "/tmp/archbridge-out".to_string();
    config
        .set("repo.demo-tool", "https://github.com/example/demo-tool")
        .expect("valid repo override");
    config.save().expect("save");

    let reloaded = Config::load_from(&path).expect("reload");
    assert!(!reloaded.aur);
    assert_eq!(reloaded.output_dir, "/tmp/archbridge-out");
    assert_eq!(
        reloaded.repos.get("demo-tool").map(String::as_str),
        Some("https://github.com/example/demo-tool")
    );
    assert_eq!(reloaded.path, path);

    // The parent directory was created, and the only file in it is the config:
    // the temporary staging file was renamed, not left behind.
    let mut entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .expect("read dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    assert_eq!(entries, vec!["config.json".to_string()], "{entries:?}");

    // Saving again replaces the file instead of failing on the existing one.
    let mut second = reloaded.clone();
    second.rpm = false;
    second.save().expect("second save");
    assert!(!Config::load_from(&path).expect("reload").rpm);
    drop(dir);
}

#[test]
fn a_truncated_file_is_reported_not_silently_replaced_by_defaults() {
    let (_dir, path) = temp_config();
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    // Exactly what a crash in the middle of the old `fs::write` produced.
    std::fs::write(&path, "{\"official\": true, \"aur\": fal").expect("write");

    let error = Config::load_from(&path).expect_err("a half-written config must be an error");
    assert!(
        error.contains(&path.display().to_string()),
        "the message must name the file: {error}"
    );
    assert!(
        error.to_lowercase().contains("not valid json"),
        "the message must say what is wrong: {error}"
    );

    std::fs::write(&path, "   ").expect("write");
    let error = Config::load_from(&path).expect_err("empty file");
    assert!(error.contains("empty"), "{error}");
}

#[test]
fn an_unknown_key_is_named_instead_of_being_ignored() {
    let (_dir, path) = temp_config();
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    // "offical" is the typo a user actually makes; ignoring it means the
    // preference they think they set never applies.
    std::fs::write(&path, "{\"offical\": false, \"aur\": true}").expect("write");
    let error = Config::load_from(&path).expect_err("unknown key");
    assert!(error.contains("offical"), "{error}");
    assert!(error.contains("aur"), "known keys must be listed: {error}");
}

#[test]
fn values_stored_in_the_file_are_revalidated_on_load() {
    let (_dir, path) = temp_config();
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");

    std::fs::write(&path, "{\"repos\": {\"x\": \"http://insecure.example/x\"}}").expect("write");
    let error = Config::load_from(&path).expect_err("a non-HTTPS repo override must not load");
    assert!(error.contains("https"), "{error}");

    std::fs::write(&path, "{\"output_dir\": \"build/out\"}").expect("write");
    let error = Config::load_from(&path).expect_err("a relative output dir must not load");
    assert!(error.contains("absolute"), "{error}");

    std::fs::write(&path, "{\"output_dir\": \"/tmp/ok\"}").expect("write");
    assert_eq!(
        Config::load_from(&path).expect("valid").output_dir,
        "/tmp/ok".to_string()
    );
}

#[test]
fn booleans_accept_the_spacing_and_case_a_person_types() {
    for (raw, expected) in [
        ("true", true),
        (" true ", true),
        ("\tTRUE\n", true),
        ("1", true),
        ("yes", true),
        ("On", true),
        ("false", false),
        ("FALSE ", false),
        ("0", false),
        (" No", false),
        ("off", false),
    ] {
        let mut config = Config::default();
        config
            .set("aur", raw)
            .unwrap_or_else(|e| panic!("'{raw}' rejected: {e}"));
        assert_eq!(config.aur, expected, "'{raw}'");
    }

    for raw in ["", " ", "maybe", "tru", "2", "yes no"] {
        let mut config = Config::default();
        let error = config
            .set("aur", raw)
            .pipe_rejected(format!("'{raw}' must not parse as a boolean"));
        assert!(error.contains("Invalid boolean"), "{error}");
    }
}

/// Several cases assert on the rejection text itself, so unwrap the error arm
/// and fail loudly when something that should have been refused slipped through.
trait Rejected {
    fn pipe_rejected(self, message: String) -> String;
}

impl<T> Rejected for Result<T, String> {
    fn pipe_rejected(self, message: String) -> String {
        match self {
            Err(error) => error,
            Ok(value) => {
                let _ = value;
                panic!("{message}")
            }
        }
    }
}

#[test]
fn repo_overrides_need_a_sane_name_and_an_https_url() {
    let good = "https://github.com/owner/repo";
    for name in ["tool", "tool_1", "tool.name", "tool+ext", "tool-1", "a@b"] {
        let mut config = Config::default();
        config
            .set(&format!("repo.{name}"), good)
            .unwrap_or_else(|e| panic!("name '{name}' rejected: {e}"));
        assert_eq!(
            config.get(&format!("repo.{name}")).expect("get").as_str(),
            Some(good)
        );
    }

    let bad_names = [
        "",
        " ",
        "has space",
        "sl/ash",
        "back\\slash",
        "emoji😀",
        "..",
        ".hidden",
        "-leading",
        &"x".repeat(200),
        "quote\"x",
        "new\nline",
        "nul\0byte",
    ];
    for name in bad_names {
        let mut config = Config::default();
        let key = format!("repo.{name}");
        let error = config
            .set(&key, good)
            .pipe_rejected(format!("repo name '{name}' was accepted"));
        assert!(error.contains("Repo name"), "{name}: {error}");
    }

    let long_url = format!("https://example/{}", "x".repeat(3000));
    for url in [
        "",
        "github.com/owner/repo",
        "http://github.com/owner/repo",
        "HTTP://github.com/owner/repo",
        "ftp://example/x",
        "file:///etc/passwd",
        "https://",
        "https:///path",
        "https://user@host/repo?q=1",
        "https://exa mple/x",
        "https://example/x\nnext",
        long_url.as_str(),
        "javascript:alert(1)",
    ] {
        let mut config = Config::default();
        let error = config
            .set("repo.demo", url)
            .pipe_rejected(format!("url '{url}' was accepted"));
        assert!(
            error.contains("Repo URL") || error.contains("scheme"),
            "{url}: {error}"
        );
        assert!(
            config.repos.is_empty(),
            "a rejected url still stored itself: {:?}",
            config.repos
        );
    }

    // Credentials in the URL are how an override turns into a redirect to
    // someone else's host, and a query/fragment hides the real path.
    for url in [
        "https://github.com/owner/repo",
        "https://gitlab.com/group/sub/project.git",
        "https://code.example.org/a/b",
    ] {
        let mut config = Config::default();
        config
            .set("repo.demo", url)
            .unwrap_or_else(|e| panic!("{url}: {e}"));
    }
}

#[test]
fn an_output_directory_is_a_usable_absolute_path_or_nothing() {
    for value in ["", "   ", "/tmp/out", "/tmp/out/"] {
        let mut config = Config::default();
        config
            .set("output_dir", value)
            .unwrap_or_else(|e| panic!("'{value}' rejected: {e}"));
    }

    let long_path = format!("/{}", "a".repeat(5000));
    for value in [
        "relative/path",
        "/",
        "/tmp/../etc",
        "/tmp/./out/../..",
        "/tmp/out\u{0}",
        "/tmp/out\nrm -rf /",
        "/tmp/\u{7}bell",
        long_path.as_str(),
    ] {
        let mut config = Config::default();
        let error = config
            .set("output_dir", value)
            .pipe_rejected(format!("'{value}' was accepted"));
        assert!(
            error.contains("Output directory"),
            "unexpected message for '{value}': {error}"
        );
        assert!(config.output_dir.is_empty(), "'{value}' was stored anyway");
    }
}

#[test]
fn key_lookup_is_exact_and_names_the_alternatives() {
    let mut config = Config::default();
    config
        .set("repo.demo", "https://github.com/owner/repo")
        .expect("repo set");

    assert_eq!(config.get("aur").expect("aur"), serde_json::json!(true));
    assert_eq!(
        config.get("repo.demo").expect("repo").as_str(),
        Some("https://github.com/owner/repo")
    );

    // Case matters, and a miss says so rather than returning another key's value.
    for key in [
        "Aur",
        "AUR",
        " all",
        "all ",
        "",
        "repo.",
        "repo.nope",
        "output",
    ] {
        let error = config
            .get(key)
            .pipe_rejected(format!("get('{key}') should not resolve"));
        assert!(
            error.contains("Unknown") || error.contains("repo") || error.contains("empty"),
            "{key}: {error}"
        );
    }
    assert!(config.get("all").expect("all").is_object());
}

/// Deterministic sweep over odd keys and values: the store may refuse them, but
/// it must never panic and never persist something it would later choke on.
#[test]
fn fuzz_key_values_never_panic_and_always_reload() {
    let keys = [
        "aur",
        "official",
        "rpm",
        "output_dir",
        "repo.x",
        "repo.",
        "repo.a b",
        "REPO.x",
        "",
        " ",
        "repos",
        "path",
        "unknown",
    ];
    let long_value = "x".repeat(5000);
    let values = [
        "",
        " ",
        "true",
        "TRUE",
        "1",
        "0",
        "no",
        "-1",
        "/tmp/out",
        "..",
        "/tmp/../out",
        "https://github.com/o/r",
        "http://github.com/o/r",
        "https://exa mple/o",
        "\u{0}",
        "\u{1f600}",
        "\"quoted\"",
        "'single'",
        "$(id)",
        "`id`",
        "a\nb",
        long_value.as_str(),
    ];

    let (_dir, path) = temp_config();
    for key in keys {
        for value in values {
            let mut config = Config {
                path: path.clone(),
                ..Config::default()
            };
            if let Ok(()) = config.set(key, value) {
                // Whatever was accepted has to survive a write/read cycle
                // unchanged, and reading it back must work too.
                config.save().expect("save accepted value");
                let reloaded = Config::load_from(&path).unwrap_or_else(|e| {
                    panic!("set('{key}','{value}') saved an unloadable file: {e}")
                });
                assert_eq!(reloaded, config, "round trip changed '{key}'");
                let _ = config.get(key);
            }
            std::fs::remove_file(&path).ok();
        }
    }
}

#[test]
fn an_unwritable_target_is_reported_without_touching_the_existing_file() {
    let (_dir, path) = temp_config();
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(&path, "{\"aur\": false}").expect("seed");

    let saved = std::fs::read_to_string(&path).expect("read");

    // Preferences cannot be written into a read-only directory. The old in-place
    // `fs::write` truncated the file first, so the attempt destroyed the copy
    // it could not replace.
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().unwrap();
    let running_as_root = std::fs::metadata(&path)
        .map(|meta| meta.uid() == 0)
        .unwrap_or(false);
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let blocked = Config {
        path: path.clone(),
        ..Config::default()
    };
    let result = blocked.save();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    if !running_as_root {
        let error = result.expect_err("a read-only directory must block the save");
        assert!(error.contains("config"), "{error}");
    }
    assert_eq!(
        std::fs::read_to_string(&path).expect("read after the failed save"),
        saved,
        "a failed save must leave the previous preferences readable"
    );
    assert!(
        Config::load_from(&path).is_ok(),
        "the file was damaged by a save that failed"
    );

    // Renaming onto a directory is a failure, not a silent overwrite.
    let blocking = parent.join("nested");
    std::fs::create_dir_all(&blocking).expect("mkdir blocking");
    let pointing_at_a_dir = Config {
        path: blocking,
        ..Config::default()
    };
    assert!(
        pointing_at_a_dir.save().is_err(),
        "renaming onto a directory must fail"
    );
}

#[test]
fn a_missing_file_is_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path: &Path = &dir.path().join("does-not-exist.json");
    let config = Config::load_from(path).expect("defaults for a missing file");
    assert!(config.aur);
    assert_eq!(config.path, path.to_path_buf());
}

#[test]
fn keep_jobs_is_a_boolean_preference_that_defaults_to_cleaning_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = Config::load_from(&path).unwrap();
    assert!(!config.keep_jobs, "cleanup is the default");
    assert_eq!(config.get("keep_jobs").unwrap(), serde_json::json!(false));
    config.set("keep_jobs", " Yes ").unwrap();
    assert!(config.keep_jobs);
    config.save().unwrap();
    assert!(Config::load_from(&path).unwrap().keep_jobs);
    assert!(config.set("keep_jobs", "maybe").is_err());
}
