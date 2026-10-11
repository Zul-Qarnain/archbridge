use archbridge::archive::FileReview;
use archbridge::build::{resolve_entry, snapshot_foreign_package};
use archbridge::config::Config;
use archbridge::engine::{foreign_entry, publish_artifact, Engine};
use archbridge::recipe::{
    detect_build_system, detect_build_system_in_archive, generate_pkgbuild, sanitize_entry_name,
    BuildSystem, PkgbuildParams,
};
use std::fs;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[test]
fn test_config_defaults_and_toggle() {
    let mut config = Config::default();
    assert!(config.official);
    assert!(config.aur);

    assert!(config.set("aur", "false").is_ok());
    assert!(!config.aur);
    assert!(!config.is_enabled("aur"));

    assert!(config
        .set("repo.mytool", "https://github.com/owner/repo")
        .is_ok());
    assert_eq!(
        config.get("repo.mytool").unwrap(),
        serde_json::Value::String("https://github.com/owner/repo".to_string())
    );
}

#[test]
fn an_output_folder_receives_the_exact_bytes_the_build_produced() {
    let temp = TempDir::new().unwrap();
    let job_dir = temp.path().join("job");
    let out_dir = temp.path().join("out");
    fs::create_dir_all(&job_dir).unwrap();
    let artifact = job_dir.join("demo-1.0.0-1-x86_64.pkg.tar.zst");
    fs::write(&artifact, b"reviewed package bytes").unwrap();

    let published = publish_artifact(&artifact, &out_dir.to_string_lossy()).unwrap();
    assert_eq!(
        published.parent().map(|p| p.canonicalize().unwrap()),
        Some(out_dir.canonicalize().unwrap()),
        "the artifact has to land in the folder the user chose"
    );
    assert_eq!(
        fs::read(&published).unwrap(),
        b"reviewed package bytes",
        "the published copy must be byte-identical"
    );
    assert_eq!(
        published
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        Some("demo-1.0.0-1-x86_64.pkg.tar.zst".to_string()),
        "the package file name has to survive the copy"
    );
    assert!(
        artifact.is_file(),
        "publishing copies: the job directory keeps what the build wrote"
    );
}

#[test]
fn an_unset_output_folder_leaves_the_artifact_where_it_was_built() {
    let temp = TempDir::new().unwrap();
    let artifact = temp.path().join("demo.pkg.tar.zst");
    fs::write(&artifact, b"bytes").unwrap();

    for unset in ["", "   "] {
        assert_eq!(
            publish_artifact(&artifact, unset).unwrap(),
            artifact,
            "no folder chosen means no copy"
        );
    }
}

#[test]
fn a_relative_output_directory_is_refused_before_it_scatters_artifacts() {
    let mut config = Config::default();
    let error = config
        .set("output_dir", "builds/here")
        .expect_err("a relative folder resolves against whatever directory the build ran from");
    assert!(error.contains("absolute path"), "{error}");

    config
        .set("output_dir", "")
        .expect("empty is the documented default");
    assert_eq!(
        config.get("output_dir").unwrap(),
        serde_json::Value::String(String::new())
    );
}

#[test]
fn test_recipe_cmake_detection() {
    let temp = TempDir::new().unwrap();
    let cmake_file = temp.path().join("CMakeLists.txt");
    fs::write(&cmake_file, "cmake_minimum_required(VERSION 3.10)").unwrap();

    let sys = detect_build_system(temp.path());
    assert_eq!(sys, BuildSystem::Cmake);

    let params = PkgbuildParams {
        name: "testpkg".to_string(),
        version: "1.0.0".to_string(),
        source_tarball: "src.tar.gz".to_string(),
        sha256_hash: "11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff".to_string(),
        dependencies: vec!["glibc".to_string()],
        entry_binary: Some("testpkg".to_string()),
        src_subdir: None,
    };

    let pkgbuild = generate_pkgbuild(&sys, &params).unwrap();
    assert!(pkgbuild.contains("pkgname='testpkg'"));
    assert!(pkgbuild.contains("cmake -B build"));
    assert!(!pkgbuild.contains("SKIP"));
}

#[test]
fn test_recipe_unsupported_detection() {
    let temp = TempDir::new().unwrap();
    let sys = detect_build_system(temp.path());
    match sys {
        BuildSystem::Unsupported(msg) => assert!(msg.contains("unsupported build system")),
        _ => panic!("Expected unsupported build system"),
    }
}

#[test]
fn test_engine_plan_preparation() {
    let mut engine = Engine::new();
    let plan = engine.prepare_search("bash", None).unwrap();
    assert_eq!(plan.action, "search");
    assert!(plan.decision.is_some());
}

#[test]
fn repository_database_is_not_mistaken_for_a_package() {
    let result = snapshot_foreign_package(std::path::Path::new("community.db"));
    assert!(matches!(result, Err(error) if error.contains("repository index")));
}

#[test]
fn imported_pkgbuild_extracts_payload_without_running_scripts() {
    let params = PkgbuildParams {
        name: "imported".to_string(),
        version: "1.0.0".to_string(),
        source_tarball: "src.tar.gz".to_string(),
        sha256_hash: "a".repeat(64),
        dependencies: vec![],
        entry_binary: None,
        src_subdir: None,
    };
    let pkgbuild = generate_pkgbuild(&BuildSystem::Imported, &params).unwrap();
    assert!(pkgbuild.contains("tar -xzf \"$srcdir/src.tar.gz\""));
    assert!(!pkgbuild.contains("preinst"));
}

fn archive_review(path: &str, content: Option<&str>) -> FileReview {
    FileReview {
        path: path.to_string(),
        sha256: "0".repeat(64),
        bytes: 0,
        executable: false,
        content: content.map(str::to_string),
    }
}

fn params_for(name: &str, version: &str) -> PkgbuildParams {
    PkgbuildParams {
        name: name.to_string(),
        version: version.to_string(),
        source_tarball: "src.tar.gz".to_string(),
        sha256_hash: "a".repeat(64),
        dependencies: vec![],
        entry_binary: None,
        src_subdir: None,
    }
}

#[test]
fn a_hosted_release_archive_reports_the_directory_to_build_inside() {
    let reviews = vec![
        archive_review(
            "qpdf-11.9.1/CMakeLists.txt",
            Some("cmake_minimum_required(VERSION 3.16)"),
        ),
        archive_review("qpdf-11.9.1/libqpdf/QPDF.cc", None),
    ];
    let (system, subdir) = detect_build_system_in_archive(&reviews);
    assert_eq!(system, BuildSystem::Cmake);
    assert_eq!(subdir.as_deref(), Some("qpdf-11.9.1"));

    let mut params = params_for("qpdf", "11.9.1");
    params.src_subdir = subdir;
    let pkgbuild = generate_pkgbuild(&system, &params).unwrap();
    assert!(pkgbuild.contains("cd \"$srcdir/qpdf-11.9.1\""));
}

#[test]
fn a_flat_source_tree_builds_at_the_srcdir_root() {
    let reviews = vec![
        archive_review(
            "Cargo.toml",
            Some("[package]\nname = \"tool\"\n\n[[bin]]\nname = \"tool\"\n"),
        ),
        archive_review("Cargo.lock", Some("version = 3\n")),
        archive_review("src/main.rs", Some("fn main() {}\n")),
    ];
    let (system, subdir) = detect_build_system_in_archive(&reviews);
    assert_eq!(system, BuildSystem::Cargo);
    assert_eq!(subdir, None);

    let pkgbuild = generate_pkgbuild(&system, &params_for("tool", "1.0.0")).unwrap();
    assert!(pkgbuild.contains("cd \"$srcdir\"\n"));
}

#[test]
fn an_unrecognized_archive_is_blocked_instead_of_guessed_at() {
    let reviews = vec![
        archive_review("docs/README.md", None),
        archive_review("tools/README.md", None),
    ];
    let (system, subdir) = detect_build_system_in_archive(&reviews);
    assert_eq!(subdir, None);
    assert!(matches!(&system, BuildSystem::Unsupported(reason)
        if reason.contains("unsupported build system")));
    assert!(generate_pkgbuild(&system, &params_for("mystery", "1.0.0")).is_err());
}

#[test]
fn a_go_module_with_commands_outside_the_root_outranks_its_makefile() {
    let reviews = vec![
        archive_review("cli-HEAD/Makefile", Some("install:\n\tcp glab /usr/bin\n")),
        archive_review(
            "cli-HEAD/go.mod",
            Some("module github.com/proglottis/glab\n"),
        ),
        archive_review("cli-HEAD/cmd/glab/main.go", Some("package main\n")),
    ];
    let (system, subdir) = detect_build_system_in_archive(&reviews);
    assert_eq!(system, BuildSystem::Go);
    assert_eq!(subdir.as_deref(), Some("cli-HEAD"));

    let mut params = params_for("glab", "HEAD");
    params.src_subdir = subdir;
    let pkgbuild = generate_pkgbuild(&system, &params).unwrap();
    assert!(pkgbuild.contains("cd \"$srcdir/cli-HEAD\""));
    assert!(pkgbuild.contains("-o \"archbridge-go-bin/\" ./..."));
}

#[test]
fn a_tar_global_header_beside_the_tree_does_not_hide_the_project_root() {
    // GitHub serves its archives with this pax bookkeeping file next to the
    // single project directory; treating it as a second root loses the prefix.
    let reviews = vec![
        archive_review("pax_global_header", Some("comment=integrated")),
        archive_review(
            "ripgrep-3fce3b5/Cargo.toml",
            Some("[package]\nname = \"ripgrep\"\n"),
        ),
        archive_review("ripgrep-3fce3b5/Cargo.lock", Some("version = 3\n")),
        archive_review("ripgrep-3fce3b5/src/main.rs", Some("fn main() {}\n")),
    ];
    let (system, subdir) = detect_build_system_in_archive(&reviews);
    assert_eq!(system, BuildSystem::Cargo);
    assert_eq!(subdir.as_deref(), Some("ripgrep-3fce3b5"));
}

#[test]
fn foreign_versions_become_a_valid_pkgver_and_epoch() {
    let deb = generate_pkgbuild(&BuildSystem::Make, &params_for("qpdf", "1:0.3.4-1")).unwrap();
    assert!(deb.contains("pkgver='0.3.4_1'"));
    assert!(deb.contains("epoch=1\n"));

    let tilde = generate_pkgbuild(&BuildSystem::Make, &params_for("qpdf", "1.0~rc1")).unwrap();
    assert!(tilde.contains("pkgver='1.0_rc1'"));

    let punctuated = generate_pkgbuild(&BuildSystem::Make, &params_for("qpdf", "..1.0")).unwrap();
    assert!(punctuated.contains("pkgver='1.0'"));
}

#[test]
fn an_entry_name_a_shell_cannot_take_verbatim_is_refused() {
    let mut params = params_for("tool", "1.0.0");
    params.entry_binary = Some("tool\"; rm -rf / #".to_string());
    let result = generate_pkgbuild(&BuildSystem::Cargo, &params);
    assert!(matches!(&result, Err(message) if message.contains("Entry binary name")));
}

#[test]
fn the_plan_reviews_the_exact_pkgbuild_bytes_it_will_stage() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)",
    )
    .unwrap();
    let mut engine = Engine::with_config_path(&temp.path().join("config.json"));
    let plan = engine
        .prepare_build(
            temp.path().to_str().unwrap(),
            Some("demo"),
            None,
            None,
            &[],
            &[],
        )
        .unwrap();

    let review = plan
        .reviews
        .iter()
        .find(|review| review.path == "PKGBUILD")
        .expect("a build plan must carry the PKGBUILD it is about to run");
    let content = review
        .content
        .clone()
        .expect("the PKGBUILD has to be readable in the plan");
    assert!(content.contains("pkgname='demo'"));
    assert_eq!(review.bytes as usize, content.len());
    assert_eq!(review.sha256.len(), 64);
    assert!(review.sha256.chars().all(|c| c.is_ascii_hexdigit()));
}

/// Engine plus a staged build plan for a throwaway local CMake project. Tests
/// about the plan lifecycle need a real staged plan without touching the user's
/// config or the network.
fn engine_with_staged_build(temp: &TempDir) -> (Engine, archbridge::engine::Plan) {
    fs::write(
        temp.path().join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)",
    )
    .unwrap();
    let mut engine = Engine::with_config_path(&temp.path().join("config.json"));
    let plan = engine
        .prepare_build(
            temp.path().to_str().unwrap(),
            Some("demo"),
            None,
            None,
            &[],
            &[],
        )
        .unwrap();
    (engine, plan)
}

#[test]
fn a_plan_older_than_the_review_window_cannot_be_executed() {
    let temp = TempDir::new().unwrap();
    let (mut engine, plan) = engine_with_staged_build(&temp);

    let staged_at = Instant::now()
        .checked_sub(Duration::from_secs(11 * 60))
        .expect("host uptime must exceed the 10 minute plan TTL to age a plan");
    engine
        .pending_plans
        .get_mut(&plan.plan_id)
        .expect("a prepared plan must be staged for execution")
        .1 = staged_at;

    let error = engine
        .execute_plan(&plan.plan_id, true)
        .expect_err("an expired review must not be executable");
    assert!(
        error.contains("expired"),
        "the refusal must say the review expired: {error}"
    );
    assert!(
        !engine.pending_plans.contains_key(&plan.plan_id),
        "an expired plan must be withdrawn, not left pending"
    );
}

#[test]
fn a_config_change_withdraws_every_other_staged_plan() {
    let temp = TempDir::new().unwrap();
    let (mut engine, build_plan) = engine_with_staged_build(&temp);

    let config_plan = engine.prepare_config_set("aur", "false").unwrap();
    engine
        .execute_plan(&config_plan.plan_id, true)
        .expect("the config change itself must run");

    assert!(
        !engine.pending_plans.contains_key(&build_plan.plan_id),
        "changing preferences must invalidate plans reviewed under the old ones"
    );
    let error = engine
        .execute_plan(&build_plan.plan_id, true)
        .expect_err("a withdrawn plan must not execute");
    assert!(error.contains("no longer staged"), "{error}");
}

#[test]
fn a_package_file_list_chooses_the_launcher_it_actually_ships() {
    let reviews = vec![
        archive_review("usr/share/applications/other.desktop", None),
        archive_review("usr/bin/nested/helper", None),
        archive_review("usr/bin/zziplib-tool", None),
        archive_review("usr/bin/zz", None),
    ];
    assert_eq!(
        foreign_entry(&reviews, "unrelated"),
        Some("zz".to_string()),
        "the shortest top-level usr/bin program is the launcher to try first"
    );
    assert_eq!(
        foreign_entry(&reviews, "zziplib-tool"),
        Some("zziplib-tool".to_string()),
        "a binary named after the package beats a shorter unrelated one"
    );
    assert_eq!(
        foreign_entry(&[archive_review("opt/Qoder/chrome-sandbox", None)], "qoder"),
        None,
        "a payload with no usr/bin entry states that instead of guessing"
    );
}

#[test]
fn an_entry_name_inferred_from_a_file_name_is_rewritten_not_refused() {
    assert_eq!(
        sanitize_entry_name("Qoder-linux-amd64 (1)"),
        "Qoder-linux-amd64__1_"
    );
    assert_eq!(sanitize_entry_name("WeChat"), "WeChat", "case must survive");
    assert_eq!(sanitize_entry_name("--help"), "help");
    assert_eq!(sanitize_entry_name("--"), "app");
}

#[test]
fn the_smoke_test_runs_the_path_the_container_will_actually_exec() {
    let reviews = vec![
        archive_review(".PKGINFO", None),
        archive_review("usr/bin/demo", None),
        archive_review("usr/share/doc/demo/README", None),
    ];
    assert_eq!(resolve_entry(&reviews, "demo"), Ok("/usr/bin/demo".into()));
    let missing = resolve_entry(&reviews, "other").expect_err("an absent entry must be refused");
    assert!(
        missing.contains("is not part of the built package"),
        "{missing}"
    );
    assert!(missing.contains("usr/bin/demo"), "{missing}");
}

#[test]
fn a_foreign_snapshot_reports_paths_sizes_and_content_digests() {
    // The fixture needs the same archive tools the inspector uses. A runner
    // without them (CI images ship no bsdtar) has to skip, not fail.
    let tool = ["bsdtar", "ar"].into_iter().find(|program| {
        std::process::Command::new(program)
            .arg("--version")
            .output()
            .is_err()
    });
    if let Some(missing) = tool {
        eprintln!("{missing} is unavailable; skipping");
        return;
    }

    let deb_dir = TempDir::new().unwrap();
    fs::create_dir_all(deb_dir.path().join("usr/bin")).unwrap();
    let launcher = deb_dir.path().join("usr/bin/demo");
    fs::write(&launcher, "#!/bin/sh\necho demo\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let payload = deb_dir.path().join("data.tar.gz");
    std::process::Command::new("bsdtar")
        .args(["-czf", "data.tar.gz", "usr"])
        .current_dir(deb_dir.path())
        .status()
        .expect("bsdtar must be available");
    assert!(payload.is_file());

    let deb = deb_dir.path().join("Demo App (1).deb");
    let status = std::process::Command::new("ar")
        .args(["rc", "Demo App (1).deb", "data.tar.gz"])
        .current_dir(deb_dir.path())
        .status()
        .expect("ar must be available");
    assert!(status.success(), "could not build the DEB fixture");

    let snap = snapshot_foreign_package(&deb).expect("a DEB payload must snapshot");
    let demo = snap
        .reviews
        .iter()
        .find(|review| review.path == "usr/bin/demo")
        .expect("the payload's own path must appear without a listing prefix");
    assert_eq!(demo.bytes, 20);
    assert!(demo.executable, "the mode bit has to survive the snapshot");
    assert_eq!(
        demo.sha256, "a5a301c60af0fd8cd3d77a140c73dd78dc87848025d499d5afcc1f2f7327572f",
        "the digest must be of the file's bytes"
    );
    assert_eq!(
        foreign_entry(&snap.reviews, "demo-app"),
        Some("demo".to_string()),
        "the entry default comes from the payload, not the archive file name"
    );
}

#[test]
fn a_blocked_plan_is_never_staged_for_execution() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("nothing-buildable");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("README"), "no build system here").unwrap();

    let mut engine = Engine::with_config_path(&temp.path().join("config.json"));
    let plan = engine
        .prepare_build(source.to_str().unwrap(), Some("demo"), None, None, &[], &[])
        .unwrap();

    let reason = plan
        .blocked
        .clone()
        .expect("a source tree with no recognized build system must be blocked");
    assert!(
        !engine.pending_plans.contains_key(&plan.plan_id),
        "a blocked plan offers nothing to consent to, so it must stay out of the staging store"
    );
    // The refusal a caller gets for a blocked plan is the staging error, which
    // buries the actual reason — so whoever prepared the plan has to surface
    // `blocked` itself rather than offer it for execution.
    let err = engine.execute_plan(&plan.plan_id, true).unwrap_err();
    assert!(err.contains("no longer staged"), "got '{err}'");
    assert!(!reason.is_empty());
}

/// An engine whose sources are all switched off answers discovery without ever
/// touching the network, which is what makes this path testable off Arch.
fn offline_engine(temp: &TempDir) -> Engine {
    let config_path = temp.path().join("config.json");
    fs::write(
        &config_path,
        r#"{"official":false,"aur":false,"flatpak":false,"appimage":false,"upstream":false,"deb":false,"rpm":false}"#,
    )
    .expect("write config");
    Engine::with_config_path(&config_path)
}

#[test]
fn install_by_name_reports_discovery_not_a_path_error() {
    // `install htop` used to fall through to prepare_build and answer
    // "not a valid local path, PKGBUILD, or HTTPS URL" for input that was
    // never meant to be a path.
    let temp = TempDir::new().expect("tempdir");
    let mut engine = offline_engine(&temp);
    let err = engine
        .prepare_install("htop")
        .expect_err("an unresolvable name must not fabricate a plan");
    assert!(
        !err.contains("not a valid local path"),
        "the old path-shaped message came back: {err}"
    );
    assert!(err.contains("htop"), "must name the target: {err}");
    assert!(
        err.contains("archbridge search htop"),
        "must point at the discovery command: {err}"
    );
    assert!(
        err.contains("archbridge install <file"),
        "must offer installing a concrete artifact: {err}"
    );
}

#[test]
fn install_by_name_lists_every_source_answer() {
    let temp = TempDir::new().expect("tempdir");
    let mut engine = offline_engine(&temp);
    let err = engine
        .prepare_install("htop")
        .expect_err("no source is enabled, so nothing resolves");
    for source in ["official", "aur", "flatpak", "appimage", "upstream"] {
        assert!(
            err.contains(source),
            "the decision for '{source}' must reach the user: {err}"
        );
    }
    assert!(err.contains("disabled"), "the reason must be shown: {err}");
}

#[test]
fn install_still_accepts_a_source_url_for_building() {
    // Installing from a repository URL is a build-then-install path; it must not
    // be swallowed by the new discovery error for bare names.
    let temp = TempDir::new().expect("tempdir");
    let mut engine = offline_engine(&temp);
    let err = engine
        .prepare_install("https://github.com/htop-dev/htop")
        .expect_err("the source lookup is disabled in this engine");
    assert!(
        err.contains("Cannot prepare a build"),
        "a URL must be routed to the build-then-install path, got: {err}"
    );
}
