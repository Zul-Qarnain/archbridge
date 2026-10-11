use archbridge::rpc::{JsonRpcRequest, RpcServer};
use serde_json::json;
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(args)
        .output()
        .expect("the archbridge binary must be runnable from tests")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Run the CLI against a throwaway preferences file with every source switched
/// off, so discovery answers from cache-free local state and the test needs no
/// network and never touches the developer's real config.
fn cli_offline(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path().join("archbridge");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::write(
        config_dir.join("config.json"),
        r#"{"official":false,"aur":false,"flatpak":false,"appimage":false,"upstream":false,"deb":false,"rpm":false}"#,
    )
    .expect("write config");
    Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(args)
        .env("XDG_CONFIG_HOME", dir.path())
        .output()
        .expect("the archbridge binary must be runnable from tests")
}

#[test]
fn install_by_name_explains_discovery_instead_of_rejecting_the_name() {
    let out = cli_offline(&["install", "htop", "--dry-run"]);
    let combined = format!("{}{}", stdout_of(&out), stderr_of(&out));
    assert_eq!(out.status.code(), Some(1), "{combined}");
    assert!(
        !combined.contains("not a valid local path"),
        "a package name is not a path: {combined}"
    );
    assert!(
        combined.contains("archbridge search htop"),
        "must point at discovery: {combined}"
    );
    assert!(
        combined.contains("htop"),
        "must name the target: {combined}"
    );
}

#[test]
fn requested_help_goes_to_stdout_with_exit_zero() {
    for args in [&["--help"][..], &["-h"][..], &["help"][..]] {
        let out = cli(args);
        assert_eq!(out.status.code(), Some(0), "{args:?} help must succeed");
        let stdout = stdout_of(&out);
        assert!(
            stdout.contains("USAGE:"),
            "{args:?} must print usage on stdout, got stdout={stdout:?} stderr={:?}",
            stderr_of(&out)
        );
        assert!(
            stderr_of(&out).is_empty(),
            "{args:?} asked for help, so nothing belongs on stderr"
        );
    }
}

#[test]
fn usage_lists_the_gui_command() {
    // `archbridge gui` is a real subcommand handled in main.rs, but it was
    // missing from the usage text, so nobody could discover the desktop app.
    let out = cli(&["--help"]);
    let usage = stdout_of(&out);
    assert!(
        usage
            .lines()
            .any(|line| line.trim() == "archbridge gui" || line.starts_with("archbridge gui ")),
        "usage must list 'archbridge gui', got:\n{usage}"
    );
}

#[test]
fn usage_on_misuse_goes_to_stderr_with_exit_two() {
    let out = cli(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_of(&out).contains("USAGE:"),
        "a usage failure must explain itself on stderr"
    );
    assert!(
        stdout_of(&out).is_empty(),
        "a usage failure must not pollute stdout"
    );

    let out = cli(&["nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("Unknown command: 'nope'"));
    assert!(stdout_of(&out).is_empty());
}

#[test]
fn every_subcommand_honours_help_with_its_own_text() {
    for command in [
        "search",
        "info",
        "inspect",
        "build",
        "install",
        "uninstall",
        "test",
        "config",
        "doctor",
        "serve",
        "gui",
    ] {
        for flag in ["--help", "-h"] {
            let out = cli(&[command, flag]);
            assert_eq!(
                out.status.code(),
                Some(0),
                "'{command} {flag}' must print help and exit 0: {}",
                stderr_of(&out)
            );
            let stdout = stdout_of(&out);
            assert!(
                stdout.contains(&format!("archbridge {command}")),
                "'{command} {flag}' must show its own usage, got:\n{stdout}"
            );
            assert!(
                stderr_of(&out).is_empty(),
                "'{command} {flag}' must not write to stderr"
            );
        }
    }
}

#[test]
fn help_does_not_run_the_command() {
    // `doctor --help` used to perform the whole diagnostic and exit 1.
    let out = cli(&["doctor", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !stdout_of(&out).contains("\"ready\""),
        "doctor ran instead of answering --help"
    );

    // `search --help` used to be read as a missing target.
    let out = cli(&["search", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!stderr_of(&out).contains("Missing target name"));
}

#[test]
fn unknown_flags_are_rejected_by_every_subcommand() {
    for args in [
        &["search", "--nope"][..],
        &["inspect", "--nope"][..],
        &["build", "--nope"][..],
        &["install", "--nope"][..],
        &["uninstall", "--nope"][..],
        &["test", "--nope"][..],
        &["doctor", "--nope"][..],
        &["serve", "--nope"][..],
        &["config", "get", "--nope"][..],
    ] {
        let out = cli(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} must be a usage error: {}",
            stderr_of(&out)
        );
        assert!(
            stderr_of(&out).contains("Unknown flag '--nope'"),
            "{args:?} must name the rejected flag, got: {}",
            stderr_of(&out)
        );
        assert!(stdout_of(&out).is_empty(), "{args:?} misuse writes stderr");
    }
}

#[test]
fn unknown_flags_are_a_usage_error_instead_of_a_silent_real_run() {
    // `--dryrun` used to fall on the floor, so the install ran for real.
    let out = cli(&["install", "--dryrun", "coreutils"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("Unknown flag '--dryrun'"),
        "expected the flag to be named: {}",
        stderr
    );
}

#[test]
fn missing_required_arguments_exit_with_usage_code() {
    for args in [
        &["build"][..],
        &["test"][..],
        &["search"][..],
        &["install"][..],
        &["inspect"][..],
        &["config", "set"][..],
    ] {
        let out = cli(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} must be reported as a usage error"
        );
    }
}

#[test]
fn a_blocked_plan_exits_3_and_never_executes() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README"), "no build system here").expect("write README");

    let out = cli(&["build", dir.path().to_str().unwrap(), "--yes"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "blocked plan: {}",
        stderr_of(&out)
    );
}

#[test]
fn a_successful_query_exits_0() {
    let out = cli(&["config", "get", "aur"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
}

#[test]
fn test_rpc_capabilities() {
    let mut server = RpcServer::new();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "v1.capabilities".to_string(),
        params: json!({}),
    };
    let resp = server.handle_request(req);
    assert!(resp.error.is_none());
    let res = resp.result.unwrap();
    assert_eq!(res.get("foreign_script_execution").unwrap(), false);
}

#[test]
fn test_rpc_config_get_set() {
    // A sandboxed config file: the previous version of this test wrote
    // `aur: false` into the developer's real ~/.config/archbridge/config.json.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.json");
    let mut server = RpcServer::with_config_path(&config_path);
    let req_set = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "v1.prepare".to_string(),
        params: json!({
            "action": "config.set",
            "key": "aur",
            "value": "false"
        }),
    };
    let resp = server.handle_request(req_set);
    assert!(resp.error.is_none());
    let plan = resp.result.unwrap();
    let plan_id = plan.get("plan_id").unwrap().as_str().unwrap();
    assert_eq!(
        plan.get("filesystem_changes")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("path"))
            .and_then(|p| p.as_str()),
        Some(config_path.to_str().unwrap()),
        "the plan must name the file execution writes"
    );

    let req_exec = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "v1.execute".to_string(),
        params: json!({
            "plan_id": plan_id,
            "confirmed": true
        }),
    };
    let resp_exec = server.handle_request(req_exec);
    assert!(resp_exec.error.is_none());

    let req_get = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "v1.config.get".to_string(),
        params: json!({ "key": "aur" }),
    };
    let resp_get = server.handle_request(req_get);
    assert_eq!(resp_get.result.unwrap(), false);

    // The change has to survive a reload, otherwise the save never happened.
    let reloaded = RpcServer::with_config_path(&config_path);
    assert!(!reloaded.engine.config.aur);
    drop(dir);
}

fn cli_with_config_home(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(args)
        .env("XDG_CONFIG_HOME", home)
        .output()
        .expect("the archbridge binary must be runnable from tests")
}

#[test]
fn config_set_takes_a_key_and_a_value_from_the_command_line() {
    // Regression: the value was reported as an "unexpected argument", so
    // `config set <key> <value>` could never succeed from the CLI.
    let home = tempfile::tempdir().unwrap();
    let out = cli_with_config_home(&["config", "set", "aur", "false", "--yes"], home.path());
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));

    let out = cli_with_config_home(&["config", "get", "aur"], home.path());
    assert_eq!(stdout_of(&out).trim(), "false");

    // A third positional is still surplus.
    let out = cli_with_config_home(&["config", "set", "aur", "false", "extra"], home.path());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn a_blank_target_is_a_usage_error_not_a_discovery() {
    let home = tempfile::tempdir().unwrap();
    for command in ["search", "info", "inspect", "install", "uninstall"] {
        for blank in ["", "   "] {
            let out = cli_with_config_home(&[command, blank], home.path());
            assert_eq!(
                out.status.code(),
                Some(2),
                "{command} {blank:?}: {}",
                stderr_of(&out)
            );
        }
    }
}

#[test]
fn double_dash_ends_option_parsing() {
    let home = tempfile::tempdir().unwrap();
    // Without `--` a leading '-' is an unknown flag (exit 2); after it, it is a value.
    let out = cli_with_config_home(&["config", "set", "output_dir", "-x"], home.path());
    assert_eq!(out.status.code(), Some(2));
    let out = cli_with_config_home(
        &["config", "set", "--dry-run", "--", "repo.x", "-weird"],
        home.path(),
    );
    assert!(
        !stderr_of(&out).contains("Unknown flag"),
        "{}",
        stderr_of(&out)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_or_closed_stdout_is_reported_not_a_panic() {
    // Regression: println! panicked (exit 101) when stdout could not be written.
    let home = tempfile::tempdir().unwrap();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full");
    let out = Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(["config", "get"])
        .env("XDG_CONFIG_HOME", home.path())
        .stdout(full)
        .output()
        .unwrap();
    let stderr = stderr_of(&out);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_unwritable_stderr_does_not_replace_the_exit_code_with_a_panic() {
    // Regression: eprintln! panics on a closed/full stderr, so a usage error
    // (exit 2) became exit 101 whenever its message could not be written.
    let home = tempfile::tempdir().unwrap();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full");
    let out = Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .arg("definitely-not-a-command")
        .env("XDG_CONFIG_HOME", home.path())
        .stderr(full)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "a usage error stays exit 2");

    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full");
    let out = Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(["config", "get", "no_such_key"])
        .env("XDG_CONFIG_HOME", home.path())
        .stderr(full)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "a failure stays exit 1");
}
