//! Which remote build sources `prepare_build` will fetch from. github.com and
//! gitlab.com resolve through release discovery; any other HTTPS host is only
//! fetched when the user vouched for it with a `repo.*` preference, only for a
//! plain `.tar.gz`/`.tgz` archive, and never for loopback/private addresses.
//! Nothing here needs the network: a fetch that is allowed through fails inside
//! `curl` against a `.invalid` host, which is how the tests tell "allowed, then
//! failed" apart from "refused before any request".

use archbridge::engine::Engine;
use tempfile::TempDir;

fn engine_with(repos: &[(&str, &str)]) -> (TempDir, Engine) {
    let temp = TempDir::new().unwrap();
    let mut engine = Engine::with_config_path(&temp.path().join("config.json"));
    for (name, url) in repos {
        engine.config.set(&format!("repo.{name}"), url).unwrap();
    }
    (temp, engine)
}

fn build(engine: &mut Engine, target: &str) -> Result<archbridge::engine::Plan, String> {
    engine.prepare_build(target, None, None, None, &[], &[])
}

const ARCHIVE: &str = "https://downloads.archbridge-test.invalid/tool/tool-1.2.tar.gz";

#[test]
fn an_unvouched_host_is_still_refused_before_any_request() {
    let (_t, mut engine) = engine_with(&[]);
    let error = build(&mut engine, ARCHIVE).expect_err("an unknown host is not fetched");
    assert!(
        error.contains("repo.<name>"),
        "must say how to allow it: {error}"
    );
    assert!(
        !error.contains("Failed to download"),
        "no request was made: {error}"
    );
}

#[test]
fn a_host_vouched_for_by_a_repo_override_is_fetched() {
    let (_t, mut engine) =
        engine_with(&[("mirror", "https://downloads.archbridge-test.invalid/index")]);
    let error = build(&mut engine, ARCHIVE).expect_err("the .invalid host cannot answer");
    assert!(
        error.contains("Failed to download") && error.contains(ARCHIVE),
        "the vouched host must reach the download step: {error}"
    );
}

#[test]
fn a_vouched_host_only_serves_plain_gzip_archives() {
    let (_t, mut engine) =
        engine_with(&[("mirror", "https://downloads.archbridge-test.invalid/index")]);
    for target in [
        "https://downloads.archbridge-test.invalid/tool.zip",
        "https://downloads.archbridge-test.invalid/tool.sh",
        "https://downloads.archbridge-test.invalid/",
    ] {
        let error = build(&mut engine, target).expect_err(target);
        assert!(!error.contains("Failed to download"), "{target}: {error}");
    }
}

#[test]
fn a_bare_name_configured_as_a_repo_stands_for_its_url() {
    let (_t, mut engine) = engine_with(&[("tool", ARCHIVE)]);
    let error = build(&mut engine, "tool").expect_err("the .invalid host cannot answer");
    assert!(
        error.contains("Failed to download") && error.contains(ARCHIVE),
        "{error}"
    );
}

#[test]
fn a_local_path_wins_over_a_configured_name() {
    let name = "archbridge-local-wins";
    let local = std::env::current_dir().unwrap().join(name);
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(
        local.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)",
    )
    .unwrap();
    let (_t, mut engine) = engine_with(&[(name, ARCHIVE)]);
    let result = build(&mut engine, name);
    let _ = std::fs::remove_dir_all(&local);
    let plan = result.expect("an existing directory is built, not the configured URL");
    assert_eq!(plan.action, "build");
}

#[test]
fn private_loopback_and_local_hosts_are_never_fetched_even_when_configured() {
    for host in [
        "127.0.0.1",
        "localhost",
        "10.0.0.5",
        "192.168.1.9",
        "169.254.169.254",
        "172.16.0.1",
        "[::1]",
        "[fd00::1]",
        "printer.local",
    ] {
        let (_t, mut engine) = engine_with(&[("lan", &format!("https://{host}/index"))]);
        let target = format!("https://{host}/tool-1.0.tar.gz");
        let error = build(&mut engine, &target).expect_err(&target);
        assert!(error.contains("Refusing to fetch"), "{host}: {error}");
    }
}

#[test]
fn credentials_and_plain_http_never_count_as_a_vouched_host() {
    let (_t, mut engine) = engine_with(&[("mirror", "https://mirror.archbridge-test.invalid/i")]);
    for target in [
        "https://user:pw@mirror.archbridge-test.invalid/tool.tar.gz",
        "http://mirror.archbridge-test.invalid/tool.tar.gz",
        "https://mirror.archbridge-test.invalid.evil.example/tool.tar.gz",
    ] {
        let error = build(&mut engine, target).expect_err(target);
        assert!(!error.contains("Failed to download"), "{target}: {error}");
    }
}
