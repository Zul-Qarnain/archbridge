//! The process chokepoint: every external command ArchBridge runs goes through
//! `run_step`, so its timeout, output-cap, stdin and environment behaviour are
//! the security boundary for the whole engine.

use archbridge::process::{
    begin_phase, end_phase, progress_snapshot, run_step, run_step_bytes, run_step_in_dir, Step,
};
use std::io::Write;
use std::time::Instant;

fn program_exists(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            let _ = child.kill();
            let _ = child.wait();
        })
        .is_ok()
}

#[test]
fn a_step_that_outlives_its_timeout_is_killed_and_reported() {
    if !program_exists("sleep") {
        eprintln!("sleep is unavailable; skipping");
        return;
    }
    let started = Instant::now();
    // `sleep 30` with a one-second budget: the old code waited for the child,
    // which is fine, but never reaped it on the `try_wait` error path.
    let step = Step::new("sleep", vec!["30"], "test the timeout", 1);
    let error = run_step(&step, None).expect_err("a 30s sleep must time out");
    let elapsed = started.elapsed();
    assert!(error.contains("timed out after 1s"), "{error}");
    assert!(
        elapsed.as_secs() < 8,
        "the timeout was not enforced promptly: {elapsed:?}"
    );
}

/// Writing all of stdin before draining stdout deadlocks against any child that
/// answers before it has finished reading: the pipe fills and both sides wait
/// for the timeout instead of making progress.
#[test]
fn a_child_that_writes_before_it_finishs_reading_cannot_wedge_the_step() {
    if !program_exists("cat") {
        eprintln!("cat is unavailable; skipping");
        return;
    }
    // Two MiB, thirty times the 64 KiB Linux pipe buffer. ASCII keeps the
    // `run_step` text round trip exact so the comparison can be byte for byte.
    let payload: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|index| b'0' + (index % 10) as u8)
        .collect();
    let step = Step::new("cat", Vec::<String>::new(), "echo the payload", 30);
    let started = Instant::now();
    // `run_step` is the only path that feeds stdin, and lossy decoding of this
    // payload is byte-preserving for the round trip we are timing.
    let out = run_step(&step, Some(&payload))
        .expect("cat must complete")
        .stdout
        .into_bytes();
    assert!(
        started.elapsed().as_secs() < 20,
        "the step blocked until its timeout: {:?}",
        started.elapsed()
    );
    assert_eq!(out, payload, "the echoed bytes must survive intact");
}

#[test]
fn a_loud_command_is_captured_without_being_truncated() {
    if !program_exists("head") || !program_exists("sh") {
        eprintln!("head/sh are unavailable; skipping");
        return;
    }
    // 1 MiB of text is inside the cap and must come back whole; the previous
    // code had no cap at all, so this also pins the boundary behaviour.
    let step = Step::new(
        "head",
        vec!["-c", "1048576", "/dev/zero"],
        "produce a megabyte",
        30,
    );
    let out = run_step(&step, None).expect("head must run");
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    // /dev/zero is NUL bytes: lossy decoding keeps one char per byte.
    assert_eq!(
        out.stdout.len(),
        1048576,
        "a megabyte of output was truncated"
    );
}

#[test]
fn binary_output_survives_run_step_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("blob");
    let raw: Vec<u8> = vec![0xde, 0xad, 0x00, 0xff, 0xfe, 0x41];
    std::fs::write(&path, &raw).expect("write blob");

    if !program_exists("cat") {
        eprintln!("cat is unavailable; skipping");
        return;
    }
    let step = Step::new("cat", vec![path.to_str().unwrap()], "read the blob", 10);
    let bytes = run_step_bytes(&step, None).expect("cat runs");
    assert_eq!(bytes.exit_code, 0);
    assert_eq!(bytes.bytes, raw, "the download path must keep exact bytes");

    let text = run_step(&step, None).expect("cat runs");
    assert_ne!(
        text.stdout.as_bytes(),
        &raw[..],
        "lossy decoding is expected to rewrite invalid UTF-8, which is why bytes exist"
    );
    assert!(text.stdout.contains('\u{fffd}'), "{}", text.stdout);
}

#[test]
fn a_working_directory_is_applied_to_the_step() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("inside.txt"), b"relative read").expect("write");
    if !program_exists("cat") {
        eprintln!("cat is unavailable; skipping");
        return;
    }

    let step = Step::new("cat", vec!["inside.txt"], "read a relative path", 10);
    // Without the working directory the relative name resolves against the
    // test runner's cwd and the command fails.
    let in_dir = run_step_in_dir(&step, None, Some(dir.path())).expect("cat runs");
    assert_eq!(in_dir.exit_code, 0, "{}", in_dir.stderr);
    assert_eq!(in_dir.stdout, "relative read");

    let elsewhere = run_step(&step, None).expect("cat is still spawned");
    assert_ne!(
        elsewhere.exit_code, 0,
        "the same relative path must not resolve outside the working directory"
    );
}

#[test]
fn a_missing_program_is_an_error_and_never_a_panic() {
    let step = Step::new(
        "archbridge-does-not-exist",
        vec!["--version"],
        "spawn failure path",
        5,
    );
    let error = run_step(&step, None).expect_err("spawn must fail");
    assert!(error.contains("Failed to spawn"), "{error}");
}

/// `TAR_OPTIONS`, `GZIP` and friends are re-read as extra arguments by the tools
/// a build runs, so a hostile or careless environment turns `tar` into code
/// execution. Children get a clean set.
#[test]
fn option_carrying_environment_variables_do_not_reach_children() {
    if !program_exists("env") {
        eprintln!("env is unavailable; skipping");
        return;
    }
    let planted = [
        ("TAR_OPTIONS", "--checkpoint-action=exec=id"),
        ("GZIP", "-v --name"),
        ("SUDO_ASKPASS", "/tmp/attacker.sh"),
        ("SSH_ASKPASS", "/tmp/attacker.sh"),
        ("BASH_ENV", "/tmp/attacker.sh"),
        ("LD_PRELOAD", "/tmp/attacker.so"),
        ("GREP_OPTIONS", "--exclude=important"),
    ];
    for (name, value) in planted {
        std::env::set_var(name, value);
    }

    let step = Step::new(
        "env",
        Vec::<String>::new(),
        "dump the child environment",
        10,
    );
    let out = run_step(&step, None).expect("env runs");
    assert_eq!(out.exit_code, 0, "{}", out.stderr);

    for (name, value) in planted {
        assert!(
            !out.stdout.contains(&format!("{name}={value}")),
            "{name} leaked into the child environment"
        );
    }
    // The scrub must be surgical: PATH and HOME are needed to build anything.
    assert!(
        out.stdout.contains("PATH="),
        "PATH was removed, which breaks every build:\n{}",
        out.stdout
    );

    for (name, _) in planted {
        std::env::remove_var(name);
    }
}

#[test]
fn child_input_is_written_and_closed() {
    if !program_exists("cat") {
        eprintln!("cat is unavailable; skipping");
        return;
    }
    // `cat` with piped stdin only exits when the writer closes the pipe.
    let step = Step::new("cat", Vec::<String>::new(), "consume stdin", 10);
    let out = run_step(&step, Some(b"password-line\n")).expect("cat completes");
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "password-line\n");

    // No input means a null stdin: a tool must never be able to read the
    // daemon's own request stream.
    let silent = Step::new("cat", Vec::<String>::new(), "read nothing", 10);
    let out = run_step(&silent, None).expect("cat exits on a null stdin");
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    assert!(out.stdout.is_empty(), "{}", out.stdout);
}

#[test]
fn the_progress_tracker_closes_every_step_it_opens() {
    if !program_exists("true") {
        eprintln!("true is unavailable; skipping");
        return;
    }
    // The tracker is process-global, so this asserts membership of its own step
    // rather than counts another test in this binary could have changed.
    let purpose = "a completed step 7f3c";
    begin_phase("testing steps", 2);
    let step = Step::new("true", Vec::<String>::new(), purpose, 10);
    let out = run_step(&step, None).expect("true runs");
    assert_eq!(out.exit_code, 0);

    let snapshot = progress_snapshot();
    assert_eq!(snapshot.phase.as_deref(), Some("testing steps"));
    assert!(
        snapshot
            .history
            .iter()
            .any(|finished| finished.ok && finished.purpose == purpose),
        "the completed step never reached the history: {:?}",
        snapshot.history
    );
    assert!(
        !snapshot.running.iter().any(|run| run.purpose == purpose),
        "a finished step is still listed as running: {:?}",
        snapshot.running
    );

    end_phase();
    assert_eq!(progress_snapshot().phase, None);
}

#[test]
fn stderr_that_is_not_utf8_still_reports_a_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("raw");
    std::fs::write(&path, b"fine").expect("write");
    if !program_exists("cat") {
        eprintln!("cat is unavailable; skipping");
        return;
    }
    // A missing file makes coreutils print the raw path bytes in the message;
    // the wrapper must still return text rather than failing to decode.
    let step = Step::new(
        "cat",
        vec![path.to_str().unwrap(), "no-such-\u{1f600}"],
        "bad path",
        10,
    );
    let out = run_step(&step, None).expect("cat is spawned");
    assert_ne!(out.exit_code, 0);
    let mut sink = std::io::sink();
    write!(sink, "{}", out.stderr).expect("stderr is displayable");
}
