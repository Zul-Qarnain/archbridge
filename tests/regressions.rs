//! Regression tests for engine, recipe and protocol defects. Each test names the
//! behaviour that used to be wrong.

use archbridge::archive::{read_tarball_entry, snapshot_directory, unpack_tarball_tree};
use archbridge::build::find_built_package;
use archbridge::doctor::{evaluate_network_probe_result, find_in_path};
use archbridge::engine::{infer_target_name, package_name_from_file, pkgbuild_pkgname, Engine};
use archbridge::process::ProcessOutput;
use archbridge::recipe::{generate_pkgbuild, is_dependency_spec, BuildSystem, PkgbuildParams};
use archbridge::rpc::RpcServer;
use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
use std::time::Duration;
use tempfile::TempDir;

fn engine(temp: &TempDir) -> Engine {
    Engine::with_config_path(&temp.path().join("config.json"))
}

#[test]
fn a_config_value_with_quotes_is_saved_exactly() {
    // Execution used to re-parse the value out of the plan's description and
    // trimmed every trailing quote.
    let temp = TempDir::new().unwrap();
    let mut engine = engine(&temp);
    let value = "https://example.org/it's' = 'x'";
    let plan = engine.prepare_config_set("repo.demo", value).unwrap();
    let result = engine.execute_plan(&plan.plan_id, true).unwrap();
    assert!(result.ok);
    assert_eq!(
        engine.config.repos.get("demo").map(String::as_str),
        Some(value)
    );
}

#[test]
fn plan_ids_are_unique_and_long() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine(&temp);
    let mut ids = HashSet::new();
    for _ in 0..200 {
        let plan = engine.prepare_config_set("aur", "true").unwrap();
        let hex = plan.plan_id.trim_start_matches("config.set-");
        assert_eq!(hex.len(), 32, "{}", plan.plan_id);
        assert!(ids.insert(plan.plan_id));
    }
}

#[test]
fn a_test_plan_refuses_a_missing_or_non_package_file() {
    // `test` used to stage a plan for anything and then report success
    // without running the smoke test at all.
    let temp = TempDir::new().unwrap();
    let mut engine = engine(&temp);
    assert!(engine
        .prepare_test("/nonexistent/foo-1-1-x86_64.pkg.tar.zst", None, &[])
        .is_err());
    let not_pkg = temp.path().join("notes.txt");
    fs::write(&not_pkg, "x").unwrap();
    assert!(engine
        .prepare_test(not_pkg.to_str().unwrap(), None, &[])
        .is_err());
}

#[test]
fn a_test_plan_infers_the_package_name_not_the_file_name() {
    let temp = TempDir::new().unwrap();
    let pkg = temp.path().join("ripgrep-14.1.0-1-x86_64.pkg.tar.zst");
    fs::write(&pkg, b"bytes").unwrap();
    let mut engine = engine(&temp);
    let plan = engine
        .prepare_test(pkg.to_str().unwrap(), None, &[])
        .unwrap();
    assert!(
        plan.summary.contains("entry binary 'ripgrep'"),
        "{}",
        plan.summary
    );
    assert_eq!(plan.reviews.len(), 1);
    assert_eq!(plan.reviews[0].sha256.len(), 64);
}

#[test]
fn a_package_changed_after_review_is_not_installed() {
    let temp = TempDir::new().unwrap();
    let pkg = temp.path().join("demo-1.0-1-x86_64.pkg.tar.zst");
    fs::write(&pkg, b"reviewed bytes").unwrap();
    let mut engine = engine(&temp);
    let plan = engine.prepare_install(pkg.to_str().unwrap()).unwrap();
    assert_eq!(plan.action, "install");
    fs::write(&pkg, b"swapped bytes").unwrap();
    let error = engine.execute_plan(&plan.plan_id, true).unwrap_err();
    assert!(error.contains("changed after it was reviewed"), "{error}");
}

#[test]
fn an_option_shaped_target_never_reaches_pacman() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine(&temp);
    let error = engine.prepare_install("--config=/tmp/evil").unwrap_err();
    assert!(error.contains("not a package name"), "{error}");
}

#[test]
fn package_names_are_read_from_artifact_file_names() {
    assert_eq!(
        package_name_from_file("python-foo-bar-1.2.3-2-any.pkg.tar.zst").as_deref(),
        Some("python-foo-bar")
    );
    assert_eq!(package_name_from_file("foo.pkg.tar.zst"), None);
}

#[test]
fn pkgbuild_names_come_from_the_pkgbuild() {
    assert_eq!(
        pkgbuild_pkgname("# x\npkgname=hello\npkgver=1\n").as_deref(),
        Some("hello")
    );
    assert_eq!(
        pkgbuild_pkgname("pkgname=('split-a' 'split-b')\n").as_deref(),
        Some("split-a")
    );
    assert_eq!(pkgbuild_pkgname("pkgname=\"$(evil)\"\n"), None);
}

#[test]
fn target_names_ignore_trailing_slashes_and_suffixes() {
    assert_eq!(infer_target_name("https://github.com/o/repo.git"), "repo");
    assert_eq!(infer_target_name("/src/project/"), "project");
    assert_eq!(infer_target_name("pkg.deb"), "pkg");
    assert!(!infer_target_name(".").is_empty());
    assert_ne!(infer_target_name("."), ".");
}

#[test]
fn a_pkgbuild_target_is_named_after_its_pkgname_and_reviews_its_sources() {
    // The plan used to be named "PKGBUILD", and the job directory received only
    // a tarball, so local sources like patches were missing at build time.
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("proj");
    fs::create_dir(&dir).unwrap();
    fs::write(
        dir.join("PKGBUILD"),
        "pkgname=hello\npkgver=1\npkgrel=1\narch=(any)\nsource=(fix.patch)\n",
    )
    .unwrap();
    fs::write(dir.join("fix.patch"), "--- a\n+++ b\n").unwrap();
    let mut engine = engine(&temp);
    let plan = engine
        .prepare_build(
            dir.join("PKGBUILD").to_str().unwrap(),
            None,
            None,
            None,
            &[],
            &[],
        )
        .unwrap();
    assert!(plan.summary.contains("'hello'"), "{}", plan.summary);
    assert!(plan.reviews.iter().any(|r| r.path == "fix.patch"));
    // No smoke test is invented for a binary the PKGBUILD never promised.
    assert!(!plan.steps.iter().any(|s| s.program == "systemd-nspawn"));
}

#[test]
fn the_reviewed_tree_unpacks_with_its_local_sources() {
    let temp = TempDir::new().unwrap();
    let src = temp.path().join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("PKGBUILD"), "pkgname=x\n").unwrap();
    fs::write(src.join("sub/fix.patch"), "patch").unwrap();
    let snap = snapshot_directory(&src).unwrap();

    let dest = temp.path().join("job");
    fs::create_dir(&dest).unwrap();
    unpack_tarball_tree(&snap.tarball_bytes, &dest, None).unwrap();
    assert_eq!(
        fs::read_to_string(dest.join("sub/fix.patch")).unwrap(),
        "patch"
    );
    assert_eq!(
        read_tarball_entry(&snap.tarball_bytes, "PKGBUILD").unwrap(),
        b"pkgname=x\n"
    );
}

fn gz_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        // Write the raw name so `..` survives into the archive.
        let name = header.as_old_mut().name.as_mut();
        name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        builder.append(&header, Cursor::new(*data)).unwrap();
    }
    let raw = builder.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &raw).unwrap();
    gz.finish().unwrap()
}

#[test]
fn an_archive_cannot_write_outside_the_job_directory() {
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("job");
    fs::create_dir(&dest).unwrap();
    let evil = gz_tar(&[("pkg/../../escape", b"x")]);
    assert!(unpack_tarball_tree(&evil, &dest, Some("pkg")).is_err());
    assert!(!temp.path().join("escape").exists());
}

#[test]
fn an_aur_snapshot_unpacks_without_its_wrapping_directory() {
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("job");
    fs::create_dir(&dest).unwrap();
    let snap = gz_tar(&[
        ("hello/PKGBUILD", b"pkgname=hello\n"),
        ("hello/a.install", b"i"),
    ]);
    unpack_tarball_tree(&snap, &dest, Some("hello")).unwrap();
    assert!(dest.join("PKGBUILD").is_file());
    assert!(dest.join("a.install").is_file());
}

#[test]
fn a_dependency_cannot_break_out_of_the_generated_pkgbuild() {
    assert!(is_dependency_spec("glibc"));
    assert!(is_dependency_spec("python>=3.11"));
    assert!(is_dependency_spec("lib32-foo=1:2.0-1"));
    assert!(!is_dependency_spec("foo'); rm -rf ~; ('"));
    assert!(!is_dependency_spec("$(id)"));
    assert!(!is_dependency_spec("-x"));
    assert!(!is_dependency_spec("foo=>1"));

    let params = PkgbuildParams {
        name: "demo".to_string(),
        version: "1.0".to_string(),
        source_tarball: "src.tar.gz".to_string(),
        sha256_hash: "0".repeat(64),
        dependencies: vec!["ok".to_string(), "bad' ; touch /tmp/pwned ; '".to_string()],
        entry_binary: Some("demo".to_string()),
        src_subdir: None,
    };
    assert!(generate_pkgbuild(&BuildSystem::Cmake, &params).is_err());
}

#[test]
fn the_real_package_is_chosen_over_its_debug_symbols() {
    let temp = TempDir::new().unwrap();
    let real = temp.path().join("demo-1-1-x86_64.pkg.tar.zst");
    fs::write(&real, b"pkg").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    // Written last, so it is the newest file in the directory.
    fs::write(
        temp.path().join("demo-debug-1-1-x86_64.pkg.tar.zst"),
        b"dbg",
    )
    .unwrap();
    let chosen = find_built_package(temp.path()).unwrap();
    assert_eq!(chosen, real.canonicalize().unwrap());
}

#[test]
fn an_ssl_timeout_is_offline_not_a_certificate_failure() {
    let out = ProcessOutput {
        exit_code: 28,
        stdout: String::new(),
        stderr: "curl: (28) SSL connection timeout".to_string(),
    };
    let result = evaluate_network_probe_result(Ok(&out), "https://archlinux.org");
    assert_eq!(result.status, "unavailable");
}

#[test]
fn tools_are_found_on_path_without_which() {
    assert!(find_in_path("sh").is_some());
    assert!(find_in_path("definitely-not-a-real-tool-xyz").is_none());
}

#[test]
fn an_oversized_rpc_frame_is_rejected_without_buffering_it_whole() {
    let temp = TempDir::new().unwrap();
    let mut server = RpcServer::with_config_path(&temp.path().join("config.json"));
    let input = vec![b'a'; 3 * 1024 * 1024];
    let mut output = Vec::new();
    let result = server.run_loop(Cursor::new(input), &mut output);
    assert!(result.is_err());
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("exceeds maximum size"), "{text}");
}

#[test]
fn config_set_accepts_its_fields_inside_request() {
    let temp = TempDir::new().unwrap();
    let mut server = RpcServer::with_config_path(&temp.path().join("config.json"));
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"v1.prepare","params":{"action":"config.set","request":{"key":"aur","value":"false"}}}"#;
    let mut output = Vec::new();
    server
        .run_loop(Cursor::new(format!("{input}\n")), &mut output)
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("\"plan_id\""), "{text}");
}

#[test]
fn config_set_from_the_cli_takes_a_key_and_a_value() {
    // The value used to be rejected as an unexpected extra target, so
    // `archbridge config set` could never succeed.
    let temp = TempDir::new().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(["config", "set", "aur", "false", "--yes"])
        .env("XDG_CONFIG_HOME", temp.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let saved = fs::read_to_string(temp.path().join("archbridge/config.json")).unwrap();
    assert!(saved.contains("\"aur\": false"), "{saved}");
}
