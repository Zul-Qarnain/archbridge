//! Structural invariants the security model depends on.
//!
//! These read the crate's own source as text. They are a supplement to the
//! behavioural tests in this directory, not a substitute for compiling it.

use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(name: &str) -> String {
    let path = root().join("src").join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e))
}

fn src_files() -> Vec<PathBuf> {
    let dir = root().join("src");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {}", dir.display(), e))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().map(|ext| ext == "rs").unwrap_or(false))
        .collect();
    files.sort();
    files
}

fn all_src_text() -> String {
    src_files()
        .iter()
        .map(|path| fs::read_to_string(path).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse every run of whitespace so a check can match a wrapped expression.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn assert_contains(haystack: &str, needle: &str, what: &str) {
    assert!(haystack.contains(needle), "{what} must contain {needle:?}");
}

fn assert_absent(haystack: &str, needle: &str, what: &str) {
    assert!(
        !haystack.contains(needle),
        "{what} must never contain {needle:?}"
    );
}

#[test]
fn manifest_and_module_files_exist() {
    let manifest = fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert_contains(&manifest, "name = \"archbridge\"", "Cargo.toml");
    let lib = source("lib.rs");
    for module in extract_pub_mods(&lib) {
        let flat = root().join("src").join(format!("{module}.rs"));
        let directory = root().join("src").join(&module).join("mod.rs");
        assert!(
            flat.is_file() || directory.is_file(),
            "Module '{module}' has no src/{module}.rs and no src/{module}/mod.rs"
        );
    }
}

fn extract_pub_mods(lib: &str) -> Vec<String> {
    let mut modules = Vec::new();
    for line in lib.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("pub mod ") {
            if let Some(name) = rest.strip_suffix(';') {
                modules.push(name.trim().to_string());
            }
        }
    }
    modules
}

#[test]
fn cli_is_rpc_client_not_packaging_engine() {
    let main = source("main.rs");
    assert_contains(&main, "\"v1.prepare\"", "main.rs");
    assert_contains(&main, "\"v1.execute\"", "main.rs");
    for forbidden in ["makepkg", "mkarchroot", "PKGBUILD="] {
        assert_absent(&main, forbidden, "main.rs");
    }
}

#[test]
fn no_foreign_script_executor() {
    let inspect = source("inspect.rs");
    assert_contains(&inspect, "executed: false", "inspect.rs");
    let squeezed = squeezed(&inspect);
    for shell in ["sh", "bash", "ldd"] {
        assert_absent(
            &squeezed,
            &format!("Step :: new (\"{shell}\""),
            "inspect.rs",
        );
        assert_absent(&squeezed, &format!("Step::new(\"{shell}\""), "inspect.rs");
    }
    assert_absent(&inspect, "Command::new", "inspect.rs");
}

#[test]
fn native_package_review_uses_pinned_bytes() {
    let build = source("build.rs");
    assert_contains(&build, "walk_tree(&payload_dir)", "build.rs");
    assert_contains(&build, "walk_tree(&root)", "build.rs");
    assert_absent(&build, "hasher.update(path_str.as_bytes())", "build.rs");
    assert_absent(&build, "\"-tvf\".to_string()", "build.rs");
    assert_contains(&source("archive.rs"), "hasher.update(&bytes)", "archive.rs");

    let inspect = source("inspect.rs");
    assert_contains(&inspect, "Sha256::digest(&bytes)", "inspect.rs");
    // The report carries a digest, never the payload: a foreign package can be
    // hundreds of megabytes and would be copied into every plan review.
    assert_absent(&inspect, "package_bytes", "inspect.rs");
}

#[test]
fn smoke_test_runs_a_binary_that_is_inside_the_test_root() {
    let build = source("build.rs");
    assert_contains(
        &build,
        "Install package into isolated test root",
        "build.rs",
    );
    assert_contains(&build, "resolve_entry(&reviews, entry_point)", "build.rs");
}

#[test]
fn runtime_launch_has_hard_isolation_flags() {
    let build = source("build.rs");
    assert_contains(&build, "--private-users=pick", "build.rs");
    assert_contains(&build, "--private-network", "build.rs");
    assert_contains(&build, "\"65534\".to_string()", "build.rs");
    assert_contains(&build, "smoke.timeout_seconds = 30", "build.rs");
    assert_absent(&build, "\"--bind=", "build.rs");
}

#[test]
fn no_unsigned_integrity_bypass() {
    let sources = all_src_text();
    for flag in [
        "--skipinteg",
        "--skippgpcheck",
        "--nodeps",
        "--overwrite",
        "SigLevel = Never",
    ] {
        assert_absent(&sources, flag, "src/");
    }
}

#[test]
fn dry_run_stops_before_rpc_execution() {
    let main = source("main.rs");
    let stop = main
        .find("if dry_run { return Ok(0); }")
        .expect("main.rs must return before executing on --dry-run");
    let execute = main
        .find("client.call(\"v1.execute\"")
        .expect("main.rs must call v1.execute");
    assert!(
        stop < execute,
        "the dry-run short circuit at {stop} must precede the v1.execute call at {execute}"
    );
}

#[test]
fn no_phase_two_history_collection() {
    let sources = all_src_text();
    assert_absent(&sources, "history.json", "src/");
    assert_absent(&sources, "compatibility_score", "src/");
}

#[test]
fn plan_consent_expires_and_is_voided_by_config_changes() {
    let engine = source("engine.rs");
    let squeezed = squeezed(&engine);
    assert_contains(&squeezed, "const PLAN_TTL:", "engine.rs");
    assert_contains(&squeezed, "Duration::from_secs(10 * 60)", "engine.rs");
    assert_contains(&squeezed, "created.elapsed() >= PLAN_TTL", "engine.rs");
    // One staging point only: a second raw insert would age no entry out and
    // leave a reviewed plan executable forever.
    assert_eq!(
        count_occurrences(&squeezed, "pending_plans .insert("),
        1,
        "only stage_plan may insert into the pending plan store"
    );
    assert_contains(&squeezed, "pending_plans.clear()", "engine.rs");
}

#[test]
fn the_shipped_product_is_rust_only() {
    // The Python front-end was retired when the GPUI app landed. Nothing in the
    // repository may still launch it, or the desktop entry silently ships a
    // second, unsupported implementation of the app.
    assert!(
        !root().join("archbridge-gui.py").exists(),
        "the retired Python launcher must not come back"
    );
    let desktop = fs::read_to_string(root().join("assets").join("archbridge.desktop"));
    if let Ok(desktop) = desktop {
        assert_absent(&desktop, "python", "assets/archbridge.desktop");
        assert_contains(&desktop, "Exec=archbridge gui", "assets/archbridge.desktop");
    }
    for name in ["ci.yml", "release.yml"] {
        let path = root().join(".github").join("workflows").join(name);
        if !path.is_file() {
            continue;
        }
        let workflow = fs::read_to_string(&path).unwrap();
        assert_absent(&workflow, "setup-python", name);
        assert_absent(&workflow, "archbridge-gui.py", name);
    }
}

#[test]
fn no_test_harness_depends_on_a_python_interpreter() {
    let cargo = fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(
        !cargo.contains("pyo3") && !cargo.contains("rustpython"),
        "the build must not embed a Python interpreter"
    );
    let leftovers: Vec<String> = fs::read_dir(root().join("tests"))
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.extension().map(|ext| ext == "py").unwrap_or(false))
                .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "tests/ must hold Rust tests only, found {leftovers:?}"
    );
}
