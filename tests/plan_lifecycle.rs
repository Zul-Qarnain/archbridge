//! Plan lifecycle and input hardening for the engine: what a review is bound
//! to, what may reach an argument vector, and what happens when the preferences
//! file cannot be read. Everything here is deterministic and needs no pacman,
//! makepkg, systemd-nspawn, administrator credential or network.

use archbridge::engine::Engine;
use archbridge::rpc::{RpcServer, SERVER_ERROR};
use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

/// A throwaway engine whose preferences live in `temp`, never in the real
/// `~/.config/archbridge/config.json`.
fn engine_in(temp: &TempDir) -> Engine {
    Engine::with_config_path(&temp.path().join("config.json"))
}

/// A directory with a build system the recipe layer recognizes, so a build plan
/// stages without a toolchain or a download.
fn cmake_project(temp: &TempDir) -> String {
    fs::write(
        temp.path().join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)",
    )
    .unwrap();
    temp.path().to_str().unwrap().to_string()
}

fn pkgbuild_review(plan: &archbridge::engine::Plan) -> &archbridge::archive::FileReview {
    plan.reviews
        .iter()
        .find(|review| review.path == "PKGBUILD")
        .expect("a build plan carries the PKGBUILD it will stage")
}

// ---------------------------------------------------------------- preferences

#[test]
fn a_corrupt_preferences_file_is_reported_instead_of_being_replaced_by_defaults() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.json");
    fs::write(&config, "{\"aur\": tru}").unwrap();

    let mut engine = Engine::with_config_path(&config);
    let error = engine
        .prepare_search("htop", None)
        .expect_err("a plan reviewed against guessed defaults is not a plan the user agreed to");
    assert!(error.contains("Preferences could not be read"), "{error}");
    assert!(error.contains("not valid JSON"), "{error}");
    assert!(
        error.contains(&config.to_string_lossy().to_string()),
        "the refusal has to name the file to fix: {error}"
    );
    assert!(
        error.contains("Fix or delete") || error.contains("delete"),
        "the refusal has to say what to do about it: {error}"
    );

    // The same store backs a build review: which sources discovery may query and
    // where the artifact lands both come from it.
    let project = cmake_project(&temp);
    let error = engine
        .prepare_build(&project, Some("demo"), None, None, &[], &[])
        .expect_err("a build plan needs the same preferences");
    assert!(error.contains("Preferences could not be read"), "{error}");

    assert!(
        engine
            .config_error()
            .is_some_and(|error| error.contains("not valid JSON")),
        "the remembered error is what every method reports"
    );
}

#[test]
fn a_single_key_change_never_overwrites_a_preferences_file_the_engine_cannot_read() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.json");
    let original = "{\"aur\": tru}".to_string();
    fs::write(&config, &original).unwrap();

    let mut engine = Engine::with_config_path(&config);
    let error = engine
        .prepare_config_set("aur", "false")
        .expect_err("saving now would write defaults plus one key over the user's file");
    assert!(error.contains("Preferences could not be read"), "{error}");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        original,
        "an unreadable file must stay exactly as it was until it is fixed"
    );
}

#[test]
fn an_absent_preferences_file_is_the_documented_default_not_a_failure() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);
    engine
        .prepare_config_set("aur", "false")
        .expect("no file at all means the defaults, which are a legitimate state");
    assert!(engine.config_error().is_none());
}

#[test]
fn rpc_answers_an_unreadable_preferences_file_as_a_server_error() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.json");
    fs::write(&config, "").unwrap(); // half-written file

    let mut server = RpcServer::with_config_path(&config);
    for (method, params) in [
        ("v1.config.get", json!({ "key": "aur" })),
        ("v1.prepare", json!({"action": "search", "target": "htop"})),
        (
            "v1.prepare",
            json!({"action": "config.set", "key": "aur", "value": "false"}),
        ),
    ] {
        let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response: Value = serde_json::from_str(
            &server
                .handle_frame(&frame.to_string())
                .expect("a request is answered"),
        )
        .unwrap();
        assert_eq!(
            response["error"]["code"], SERVER_ERROR,
            "{method}: {response}"
        );
        let message = response["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("empty") || message.contains("Preferences"),
            "{method} said {message:?} instead of naming the unreadable file"
        );
    }

    // An inspect review needs no preferences at all, and must stay available.
    let frame = json!({
        "jsonrpc": "2.0", "id": 2, "method": "v1.prepare",
        "params": {"action": "inspect", "target": temp.path().join("absent.deb").to_string_lossy()}
    });
    let response: Value = serde_json::from_str(
        &server
            .handle_frame(&frame.to_string())
            .expect("a request is answered"),
    )
    .unwrap();
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.contains("Preferences")),
        "inspection was blocked on preferences it never reads: {response}"
    );
}

// ------------------------------------------------------- the staged plan store

#[test]
fn the_staged_plan_store_stays_bounded_however_many_reviews_a_client_draws_up() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    let mut ids = Vec::new();
    for index in 0..200 {
        let plan = engine
            .prepare_uninstall(&format!("pkg{index}"))
            .expect("a plain package name stages a plan");
        ids.push(plan.plan_id);
    }

    assert!(
        engine.pending_plans.len() <= 32,
        "every staged plan holds a full review, so the store must be bounded: {}",
        engine.pending_plans.len()
    );
    assert!(
        engine.pending_plans.contains_key(&ids[199]),
        "the newest review must still be executable"
    );
    assert!(
        !engine.pending_plans.contains_key(&ids[0]),
        "the oldest review is the one withdrawn"
    );
    let error = engine
        .execute_plan(&ids[0], true)
        .expect_err("a withdrawn plan cannot execute");
    assert!(error.contains("no longer staged"), "{error}");
}

#[test]
fn a_config_value_is_saved_exactly_as_it_was_reviewed() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.json");
    let mut engine = Engine::with_config_path(&config);

    // The plan describes the change as "Update configuration key 'k' = 'v'". A
    // value that ends in a quote, or that holds that same separator, is destroyed
    // when execution reads it back out of the sentence: the file ends up holding
    // a path nobody reviewed, and the install goes somewhere else.
    for value in [
        format!("{}/out'", temp.path().display()),
        format!("{}/a' = 'b", temp.path().display()),
        format!("{}/trimmed  ", temp.path().display()),
    ] {
        let plan = engine
            .prepare_config_set("output_dir", &value)
            .expect("an absolute output directory is a legal preference");
        assert!(
            plan.filesystem_changes[0].description.contains("' = '"),
            "the review text is the ambiguous shape: {:?}",
            plan.filesystem_changes[0].description
        );

        engine
            .execute_plan(&plan.plan_id, true)
            .expect("the confirmed change applies");
        let saved = archbridge::config::Config::load_from(&config)
            .expect("the written file is readable again")
            .output_dir;
        assert_eq!(
            saved,
            value.trim(),
            "execution wrote exactly what the plan showed"
        );
    }
}

#[test]
fn a_plan_is_executed_once_and_a_stranger_id_is_refused() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.json");
    let mut engine = Engine::with_config_path(&config);

    let unknown = engine
        .execute_plan("config.set-000000000000", true)
        .expect_err("an id the engine never staged addresses nothing");
    assert!(unknown.contains("no longer staged"), "{unknown}");

    let plan = engine
        .prepare_config_set("aur", "false")
        .expect("a config plan stages against a temp file");

    engine
        .execute_plan(&plan.plan_id, false)
        .expect_err("consent is the whole point of a plan");
    assert!(
        engine.pending_plans.contains_key(&plan.plan_id),
        "a refusal for lack of consent must not consume the review"
    );

    engine
        .execute_plan(&plan.plan_id, true)
        .expect("the confirmed plan executes");
    let again = engine
        .execute_plan(&plan.plan_id, true)
        .expect_err("single use is what makes the confirmation mean something");
    assert!(again.contains("no longer staged"), "{again}");
    assert!(
        !archbridge::config::Config::load_from(&config)
            .expect("the temp file is readable")
            .aur,
        "the staged change is what got applied"
    );
}

#[test]
fn execute_acts_on_the_stored_plan_and_not_on_a_field_the_client_resends() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("config.json");
    let mut server = RpcServer::with_config_path(&config);

    let prepared: Value = serde_json::from_str(
        &server
            .handle_frame(
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": "v1.prepare",
                    "params": {"action": "config.set", "key": "aur", "value": "false"}
                })
                .to_string(),
            )
            .expect("a request is answered"),
    )
    .unwrap();
    let plan_id = prepared["result"]["plan_id"]
        .as_str()
        .expect("a staged plan id")
        .to_string();

    // The execute method takes a plan id and consent, nothing else: an `action`
    // bolted onto the request cannot turn a reviewed config change into a
    // privileged install.
    let response: Value = serde_json::from_str(
        &server
            .handle_frame(
                &json!({
                    "jsonrpc": "2.0", "id": 2, "method": "v1.execute",
                    "params": {"plan_id": plan_id, "confirmed": true, "action": "install"}
                })
                .to_string(),
            )
            .expect("a request is answered"),
    )
    .unwrap();
    let message = response["result"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("Configuration key"),
        "the stored action is the authority: {response}"
    );
    assert!(
        response["result"]["next_plan"].is_null(),
        "no install plan was conjured from the request: {response}"
    );
}

#[test]
fn a_review_is_bound_to_the_options_it_shown_not_just_the_target() {
    let temp = TempDir::new().unwrap();
    let project = cmake_project(&temp);
    let mut engine = engine_in(&temp);

    let first = engine
        .prepare_build(&project, Some("demo"), Some("1.0.0"), None, &[], &[])
        .expect("a cmake project stages");
    let second = engine
        .prepare_build(&project, Some("demo"), Some("2.0.0"), None, &[], &[])
        .expect("the same project at another version stages");

    assert_ne!(
        first.plan_id, second.plan_id,
        "two reviews differing only in version must not share an id"
    );
    assert_ne!(
        pkgbuild_review(&first).sha256,
        pkgbuild_review(&second).sha256,
        "and the bytes they pinned have to differ too"
    );

    let with_dep = engine
        .prepare_build(
            &project,
            Some("demo"),
            Some("1.0.0"),
            None,
            &["glibc".to_string()],
            &[],
        )
        .expect("a dependency list stages");
    assert_ne!(
        pkgbuild_review(&first).sha256,
        pkgbuild_review(&with_dep).sha256,
        "a different dependency set is a different review"
    );

    let with_args = engine
        .prepare_build(
            &project,
            Some("demo"),
            Some("1.0.0"),
            None,
            &[],
            &["--help".to_string()],
        )
        .expect("smoke arguments stage");
    let smoke_step = with_args
        .steps
        .iter()
        .find(|step| step.program == "systemd-nspawn")
        .expect("a cmake build stages a smoke test");
    assert_eq!(
        smoke_step.args.last().map(String::as_str),
        Some("--help"),
        "what the container passes to the binary is part of the review: {:?}",
        smoke_step.args
    );
}

#[test]
fn a_pkgbuild_named_without_a_directory_is_read_from_the_working_directory() {
    // `archbridge build PKGBUILD` is the ordinary way to build the recipe in
    // front of you; its parent path is empty, which is not a directory that
    // exists. This test changes the process working directory, so it is the only
    // one in this binary that takes a relative target.
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("PKGBUILD"),
        "pkgname=demo\npkgver=1.0.0\narch=('x86_64')\n",
    )
    .unwrap();
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(temp.path()).expect("chdir to the temp project");
    let staged = std::panic::catch_unwind(|| {
        let mut engine = Engine::with_config_path(&temp.path().join("config.json"));
        engine.prepare_build("PKGBUILD", Some("demo"), None, None, &[], &[])
    });
    std::env::set_current_dir(previous).expect("restore the working directory");

    let plan = staged
        .expect("preparing a bare PKGBUILD must not panic")
        .expect("the PKGBUILD in the working directory is a buildable target");
    assert!(plan
        .reviews
        .iter()
        .any(|review| review.path == "PKGBUILD" || review.path == "./PKGBUILD"));
}

// ---------------------------------------------------------- pacman option shape

#[test]
fn a_package_name_shaped_like_a_pacman_option_never_reaches_the_command() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    // `pacman -Rns --config=/tmp/x` reads the configuration, and therefore the
    // hook directory, from wherever the name points: a name that starts with '-'
    // is a flag, not a package.
    for hostile in [
        "-Rns",
        "--config=/tmp/evil",
        "-r",
        "--dbpath=/tmp/db",
        "hello world",
        "a/b",
        "../hello",
        "héllo",
        "line\nbreak",
        "",
        "   ",
        &"x".repeat(200),
        "\u{0}",
    ] {
        let error = engine
            .prepare_uninstall(hostile)
            .expect_err("{hostile:?} is not an installed package name");
        assert!(
            error.contains("installed Arch package name"),
            "{hostile:?} refused with {error}"
        );
    }

    let plan = engine
        .prepare_uninstall("hello")
        .expect("a real name stages a plan");
    let args = &plan.steps[0].args;
    assert_eq!(
        args.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["-n", "pacman", "-Rns", "--noconfirm", "hello"],
        "the plan runs exactly the reviewed, non-interactive removal"
    );
    assert!(
        !args.last().unwrap().starts_with('-'),
        "the value pacman acts on is a name, never a flag: {args:?}"
    );
}

#[test]
fn an_install_names_only_a_package_file_that_actually_exists() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    let error = engine
        .prepare_install("--config=/tmp/evil.pkg.tar.zst")
        .expect_err("a leading '-' is a pacman option, not a package path");
    assert!(error.contains("options"), "{error}");

    let error = engine
        .prepare_install(temp.path().join("absent.pkg.tar.zst").to_str().unwrap())
        .expect_err("consent cannot be about installing nothing");
    assert!(error.contains("does not exist"), "{error}");

    let artifact = temp.path().join("demo-1.0.0-1-x86_64.pkg.tar.zst");
    fs::write(
        &artifact,
        b"not really a package, but it is the bytes on disk",
    )
    .unwrap();
    let plan = engine
        .prepare_install(artifact.to_str().unwrap())
        .expect("an existing package file stages an install");
    let args = &plan.steps[0].args;
    assert_eq!(
        args.last().map(String::as_str),
        Some(artifact.to_str().unwrap()),
        "{args:?}"
    );
    assert_eq!(args[0], "-n", "the plan stays non-interactive: {args:?}");
    assert!(
        !args.iter().any(|arg| arg.starts_with("--skipinteg")
            || arg.starts_with("--nodeps")
            || arg.starts_with("--overwrite")),
        "no integrity bypass: {args:?}"
    );
}

// ------------------------------------------------------------- test plan honesty

#[test]
fn a_test_plan_cannot_be_staged_over_a_missing_or_unusable_artifact() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    let absent = temp.path().join("demo-1.0.0-1-x86_64.pkg.tar.zst");
    let error = engine
        .prepare_test(absent.to_str().unwrap(), Some("demo"), &[])
        .expect_err("a test over bytes that are not there verifies nothing");
    assert!(error.contains("is not a file"), "{error}");

    fs::create_dir(temp.path().join("fake.pkg.tar.zst")).unwrap();
    let error = engine
        .prepare_test(
            temp.path().join("fake.pkg.tar.zst").to_str().unwrap(),
            Some("demo"),
            &[],
        )
        .expect_err("a directory cannot be unpacked into a test root");
    assert!(error.contains("is not a file"), "{error}");

    let not_a_package = temp.path().join("demo");
    fs::write(&not_a_package, b"#!/bin/sh\n").unwrap();
    let error = engine
        .prepare_test(not_a_package.to_str().unwrap(), Some("demo"), &[])
        .expect_err("there is no payload to unpack from a loose file");
    assert!(error.contains(".pkg.tar."), "{error}");
}

/// A real (uncompressed) `.pkg.tar` holding one executable, `usr/bin/demo`.
fn write_demo_package(path: &std::path::Path) {
    let file = fs::File::create(path).unwrap();
    let mut builder = tar::Builder::new(file);
    let body = b"#!/bin/sh\necho demo\n";
    let mut header = tar::Header::new_gnu();
    header.set_path("usr/bin/demo").unwrap();
    header.set_size(body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append(&header, &body[..]).unwrap();
    builder.finish().unwrap();
}

#[test]
fn a_test_plan_reports_what_the_container_actually_said() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    // Not a real archive: the smoke test cannot get a payload out of it, so the
    // plan must refuse at review time instead of staging a test that "passes".
    let artifact = temp.path().join("demo-1.0.0-1-any.pkg.tar.zst");
    fs::write(&artifact, b"garbage that is not a zstd stream").unwrap();
    let error = engine
        .prepare_test(artifact.to_str().unwrap(), Some("demo"), &[])
        .expect_err("an archive with no readable payload cannot be tested");
    assert!(error.contains("unpack"), "{error}");

    // A real archive stages a plan whose review shows the isolation flags.
    let real = temp.path().join("demo-1.0.0-1-any.pkg.tar.zst");
    write_demo_package(&real);
    let plan = engine
        .prepare_test(real.to_str().unwrap(), Some("demo"), &[])
        .expect("a real package stages a test");
    assert_eq!(plan.action, "test");
    assert!(
        plan.steps[0]
            .args
            .iter()
            .any(|arg| arg == "--private-users=pick"),
        "the isolation flags are part of the review: {:?}",
        plan.steps[0].args
    );
    assert!(
        plan.reviews
            .iter()
            .any(|review| review.path == "usr/bin/demo"),
        "the reviewed file list names the entry"
    );
}

#[test]
fn a_test_entry_that_reads_as_a_container_option_is_refused() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);
    let artifact = temp.path().join("demo-1.0.0-1-any.pkg.tar.zst");
    write_demo_package(&artifact);

    for hostile in [
        "--bind=/etc",
        "-D",
        "../escape",
        "usr/bin/demo",
        "two words",
        "",
    ] {
        let error = engine
            .prepare_test(artifact.to_str().unwrap(), Some(hostile), &[])
            .expect_err("{hostile:?} is not a plain entry name");
        assert!(error.contains("Entry binary name"), "{hostile:?}: {error}");
    }

    let plan = engine
        .prepare_test(artifact.to_str().unwrap(), None, &["--version".to_string()])
        .expect("no entry means the launcher the archive ships");
    let args = &plan.steps[0].args;
    let after_root = args.iter().position(|a| a == "-D").unwrap() + 2;
    assert!(
        args[after_root].ends_with("demo") && args[after_root + 1] == "--version",
        "the entry and the arguments it is launched with are both reviewed: {args:?}"
    );
}

// ------------------------------------------------------------ build target shape

#[test]
fn a_build_entry_that_reads_as_a_container_option_is_refused() {
    let temp = TempDir::new().unwrap();
    let project = cmake_project(&temp);
    let mut engine = engine_in(&temp);

    for hostile in ["--bind=/etc", "-D", "./demo", "usr/bin/demo", ""] {
        let error = engine
            .prepare_build(&project, Some("demo"), None, Some(hostile), &[], &[])
            .expect_err("{hostile:?} would reach the container launch");
        assert!(error.contains("Entry binary name"), "{hostile:?}: {error}");
    }
}

#[test]
fn a_target_that_is_not_one_clean_argument_is_refused_before_it_reaches_a_tool() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    for hostile in [
        "",
        "   ",
        "-x",
        "--config=/tmp/evil",
        "a\u{0}b",
        "line\nbreak",
        &format!("/tmp/{}", "deep/".repeat(2000)),
    ] {
        let error = engine
            .prepare_build(hostile, Some("demo"), None, None, &[], &[])
            .expect_err("a hostile target is not something to build");
        assert!(error.contains("Target"), "{hostile:?}: {error}");
    }
}

#[test]
fn an_empty_or_hostile_option_is_refused_rather_than_silently_defaulted() {
    let temp = TempDir::new().unwrap();
    let project = cmake_project(&temp);
    let mut engine = engine_in(&temp);

    for (option, expectation) in [("", "empty"), (" ", "whitespace")] {
        let error = engine
            .prepare_build(&project, Some(option), None, None, &[], &[])
            .expect_err("an empty or blank name cannot name a package");
        assert!(
            error.contains("Package name") && error.contains(expectation),
            "{option:?}: {error}"
        );
    }

    let error = engine
        .prepare_build(&project, Some("demo"), Some(""), None, &[], &[])
        .expect_err("an empty version quietly becomes 1.0.0, which is not what was asked");
    assert!(error.contains("Package version"), "{error}");

    let error = engine
        .prepare_build(
            &project,
            Some("demo"),
            None,
            None,
            &[],
            &["two\nlines".to_string()],
        )
        .expect_err("one argv element cannot hold a newline");
    assert!(error.contains("Smoke-test arguments"), "{error}");
}

#[test]
fn a_source_that_is_not_a_supported_shape_is_named_back_to_the_caller() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);

    for (target, expectation) in [
        ("file:///etc/passwd", "not a valid local path"),
        (
            "git+https://github.com/owner/tool",
            "not a valid local path",
        ),
        ("http://github.com/owner/tool", "not a valid local path"),
        ("https://", "github.com and gitlab.com"),
        (
            "https://example.com/a/b.tar.gz",
            "github.com and gitlab.com",
        ),
        (
            "https://user:pass@github.com/a/b",
            "github.com and gitlab.com",
        ),
        ("/no/such/project", "not a valid local path"),
    ] {
        let error = engine
            .prepare_build(target, None, None, None, &[], &[])
            .expect_err("{target} cannot be built");
        assert!(
            error.contains(expectation),
            "{target} said {error:?}, expected it to explain {expectation:?}"
        );
    }
}

#[test]
fn a_hostile_dependency_is_a_blocked_plan_not_a_runnable_one() {
    let temp = TempDir::new().unwrap();
    let project = cmake_project(&temp);
    let mut engine = engine_in(&temp);

    let plan = engine
        .prepare_build(
            &project,
            Some("demo"),
            None,
            None,
            &["; rm -rf / #".to_string()],
            &[],
        )
        .expect("the reason reaches the caller as a plan");
    let reason = plan
        .blocked
        .clone()
        .expect("a dependency a PKGBUILD cannot hold blocks the plan");
    assert!(reason.contains("Dependency"), "{reason}");
    assert!(
        !engine.pending_plans.contains_key(&plan.plan_id),
        "a blocked plan is never staged"
    );
    assert!(plan.steps.is_empty(), "a blocked plan runs nothing");
}

// ----------------------------------------------------------------- fuzz sweep

/// Odd input must produce an error or an honest plan, never a panic and never a
/// staged plan whose argument vector holds something the caller did not name.
#[test]
fn odd_strings_never_panic_and_never_stage_an_unreviewable_argument() {
    let corpus: Vec<String> = vec![
        String::new(),
        " ".to_string(),
        "\t".to_string(),
        "-".to_string(),
        "--".to_string(),
        "..".to_string(),
        ".".to_string(),
        "/".to_string(),
        "///".to_string(),
        "$(reboot)".to_string(),
        "`id`".to_string(),
        "a\nb".to_string(),
        "a\rb".to_string(),
        "a\u{0}b".to_string(),
        "\u{1b}[31mred\u{1b}[0m".to_string(),
        "%n%n%n%n".to_string(),
        "🐍💀".to_string(),
        "x".repeat(5000),
        "a".repeat(300),
        "-Rns".to_string(),
        "--config=/tmp/evil".to_string(),
        "usr/bin/;ls".to_string(),
        "../../../etc/pacman.conf".to_string(),
        "demo.pkg.tar.zst".to_string(),
        "demo.deb".to_string(),
        "demo.rpm".to_string(),
        "demo.db".to_string(),
        "PKGBUILD".to_string(),
        "file:///etc/passwd".to_string(),
        "git+https://github.com/owner/tool".to_string(),
        "http://github.com/owner/tool".to_string(),
        "https://".to_string(),
        "https://github.com/".to_string(),
        "https://user:pass@github.com/a/b".to_string(),
        "   spaced name   ".to_string(),
        "two words".to_string(),
    ];

    let temp = TempDir::new().unwrap();
    let project = cmake_project(&temp);
    let artifact = temp.path().join("demo-1.0.0-1-any.pkg.tar.zst");
    fs::write(&artifact, b"placeholder bytes").unwrap();
    let mut engine = engine_in(&temp);

    for target in &corpus {
        // Every one of these is called for a value that cannot be honored, so a
        // panic here would abort the binary; the assertions below are the fallback.
        let _ = engine.prepare_build(target, Some("demo"), None, None, &[], &[]);
        let _ = engine.prepare_build(
            &project,
            Some(target.as_str()),
            Some(target.as_str()),
            Some(target.as_str()),
            std::slice::from_ref(target),
            std::slice::from_ref(target),
        );
        let _ = engine.prepare_test(target, Some(target.as_str()), std::slice::from_ref(target));
        let _ = engine.prepare_uninstall(target);
        let _ = engine.prepare_config_set(target, target);
        let _ = engine.prepare_inspect(target);
        if target.contains(".pkg.tar.") || target.starts_with('-') {
            // Both shapes stop before discovery, which is the only part that may
            // reach out to the network.
            let _ = engine.prepare_install(target);
        }

        for (plan, _, staged_build) in engine.pending_plans.values() {
            let (entry, smoke_args): (&str, &[String]) = match staged_build {
                Some(archbridge::engine::StagedAction::Build(build)) => {
                    (&build.entry, &build.smoke_args)
                }
                Some(archbridge::engine::StagedAction::Test(test)) => {
                    (&test.entry, &test.smoke_args)
                }
                None => ("", &[]),
            };
            assert!(
                !entry.contains(char::is_control),
                "a staged entry holds a control character: {:?}",
                entry
            );
            assert!(
                smoke_args.iter().all(|arg| !arg.contains(char::is_control)),
                "a staged smoke argument holds a control character"
            );
            for step in &plan.steps {
                assert!(
                    step.args.iter().all(|arg| !arg.contains(char::is_control)),
                    "a staged argument holds a control character: {:?}",
                    step.args
                );
            }
        }
        assert!(
            engine.pending_plans.len() <= 32,
            "the store stayed bounded through the sweep"
        );
    }
    assert!(
        !engine.pending_plans.is_empty(),
        "the sweep was not entirely refused: at least the clean shapes staged"
    );
}

#[test]
fn an_uninstall_rejection_says_what_is_wrong_with_the_name() {
    let temp = TempDir::new().unwrap();
    let mut engine = engine_in(&temp);
    for (name, reason) in [
        ("-Rns", "pacman option"),
        (".hidden", "cannot start with '.'"),
        ("a/b", "path or file"),
        ("héllo", "ASCII letters"),
        ("hello world", "ASCII letters"),
        ("", "empty"),
    ] {
        let error = engine.prepare_uninstall(name).unwrap_err();
        assert!(error.contains(reason), "{name:?}: {error}");
    }
    assert!(engine.prepare_uninstall("lib32-foo_bar+baz@1.2").is_ok());
}

#[test]
fn a_search_plan_summary_names_the_source_without_debug_noise() {
    let temp = TempDir::new().unwrap();
    // Every source off, so nothing can be recommended and no network is used.
    let mut engine = engine_in(&temp);
    for key in [
        "official", "aur", "flatpak", "appimage", "upstream", "deb", "rpm",
    ] {
        engine.config.set(key, "false").unwrap();
    }
    let plan = engine.prepare_search("htop", None).unwrap();
    assert!(
        plan.summary.contains("recommended source is none"),
        "{}",
        plan.summary
    );
    assert!(!plan.summary.contains("Some("), "{}", plan.summary);
    assert!(!plan.summary.contains("None"), "{}", plan.summary);
}

#[test]
fn a_build_named_by_dot_or_trailing_slash_uses_the_directory_name() {
    // Regression: `build .`, `build ..` and `build dir/` inferred an empty name
    // and silently produced a package called "imported-package".
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("my-tool");
    fs::create_dir_all(project.join("sub")).unwrap();
    fs::write(
        project.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)",
    )
    .unwrap();
    let mut engine = engine_in(&temp);
    let dir = project.display().to_string();
    for target in [
        dir.clone(),
        format!("{dir}/"),
        format!("{dir}//"),
        format!("{dir}/sub/.."),
        format!("{dir}/./"),
    ] {
        let plan = engine
            .prepare_build(&target, None, None, None, &[], &[])
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        assert!(
            pkgbuild_review(&plan)
                .content
                .as_deref()
                .unwrap()
                .contains("pkgname='my-tool'"),
            "{target}: {}",
            pkgbuild_review(&plan).content.as_deref().unwrap()
        );
    }
}
