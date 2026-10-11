use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Live step tracker: lets in-process consumers (the GUI) render real-time
// progress for long engine work without extra IPC. run_step is the chokepoint
// for every external command, so registering here captures all activity.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RunningStep {
    pub purpose: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct FinishedStep {
    pub purpose: String,
    pub elapsed_ms: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressSnapshot {
    pub phase: Option<String>,
    /// 0 means the step count is unknown (indeterminate progress).
    pub total_steps: usize,
    pub completed_steps: usize,
    pub elapsed_ms: u64,
    pub running: Vec<RunningStep>,
    pub history: Vec<FinishedStep>,
}

#[derive(Default)]
struct Tracker {
    phase: Option<(String, Instant, usize)>,
    running: Vec<(u64, String, Instant)>,
    history: Vec<FinishedStep>,
}

static TRACKER: OnceLock<Mutex<Tracker>> = OnceLock::new();

fn tracker() -> std::sync::MutexGuard<'static, Tracker> {
    TRACKER
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Start a new named phase. `total_steps` may be 0 for unknown work.
pub fn begin_phase(phase: &str, total_steps: usize) {
    let mut t = tracker();
    t.phase = Some((phase.to_string(), Instant::now(), total_steps));
    t.running.clear();
    t.history.clear();
}

/// Adjust the expected step count mid-phase (e.g. when a fast path skips
/// the remaining expensive steps).
pub fn set_phase_total(total_steps: usize) {
    let mut t = tracker();
    if let Some((name, started, _)) = t.phase.take() {
        t.phase = Some((name, started, total_steps));
    }
}

/// Add newly discovered steps to the current phase's expected count.
pub fn bump_phase_total(by: usize) {
    let mut t = tracker();
    if let Some((name, started, total)) = t.phase.take() {
        t.phase = Some((name, started, total + by));
    }
}

pub fn end_phase() {
    tracker().phase = None;
}

pub fn progress_snapshot() -> ProgressSnapshot {
    let t = tracker();
    let (phase, elapsed_ms, total, completed) = match &t.phase {
        Some((name, started, total)) => (
            Some(name.clone()),
            started.elapsed().as_millis() as u64,
            *total,
            t.history.len(),
        ),
        None => (None, 0, 0, 0),
    };
    ProgressSnapshot {
        phase,
        total_steps: total,
        completed_steps: completed.min(total.max(completed)),
        elapsed_ms,
        running: t
            .running
            .iter()
            .map(|(_, purpose, started)| RunningStep {
                purpose: purpose.clone(),
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
            .collect(),
        history: t.history.clone(),
    }
}

fn tracker_start(id: u64, purpose: &str) {
    tracker()
        .running
        .push((id, purpose.to_string(), Instant::now()));
}

fn tracker_finish(id: u64, purpose: &str, started: Instant, ok: bool) {
    let mut t = tracker();
    t.running.retain(|(rid, _, _)| *rid != id);
    if t.phase.is_some() {
        t.history.push(FinishedStep {
            purpose: purpose.to_string(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            ok,
        });
        // Keep the visible history bounded; old entries just scroll off.
        if t.history.len() > 32 {
            let excess = t.history.len() - 32;
            t.history.drain(0..excess);
        }
    }
}

static NEXT_STEP_ID: AtomicU64 = AtomicU64::new(1);

/// Human-readable elapsed duration for UI display.
pub fn format_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// Bytes a single stream may contribute to a captured result. A command that
/// prints more than this is either a build log nobody will read or a stream
/// designed to exhaust the daemon's memory.
const MAX_CAPTURED_BYTES: usize = 512 * 1024 * 1024;
/// Timeout used when a step does not name one.
const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
/// Upper bound on a step's own timeout, so a bogus plan value cannot wedge the
/// server for days.
const MAX_TIMEOUT_SECONDS: u64 = 6 * 60 * 60;

/// Environment variables a tool reinterprets as extra command-line arguments or
/// as a program to execute. They are removed from every child: `TAR_OPTIONS` and
/// `GZIP` are the classic way a hostile shell profile turns `tar` into code
/// execution, and `*_ASKPASS` hands a GUI helper the user's credentials.
const CONTAMINATING_ENV: &[&str] = &[
    "TAR_OPTIONS",
    "GZIP",
    "BZIP2",
    "XZ_OPT",
    "ZIPFLAGS",
    "UNZIP",
    "GREP_OPTIONS",
    "SUDO_ASKPASS",
    "SSH_ASKPASS",
    "LD_PRELOAD",
    "BASH_ENV",
    "ENV",
    "CDPATH",
    "PROMPT_COMMAND",
];

/// Read a stream up to a hard byte budget. The second field is the reason the
/// read stopped early, if it did, so the caller can report it instead of
/// handing back a truncated log that looks complete.
fn read_capped(source: &mut dyn std::io::Read, limit: usize) -> (Vec<u8>, Option<String>) {
    let mut out: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match source.read(&mut buffer) {
            Ok(0) => return (out, None),
            Ok(read) => {
                if out.len() + read > limit {
                    return (out, Some(format!("it produced more than {limit} bytes")));
                }
                out.extend_from_slice(&buffer[..read]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return (out, Some(e.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub program: String,
    pub args: Vec<String>,
    pub purpose: String,
    pub timeout_seconds: u64,
}

impl Step {
    pub fn new(
        program: impl Into<String>,
        args: Vec<impl Into<String>>,
        purpose: impl Into<String>,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(|a| a.into()).collect(),
            purpose: purpose.into(),
            timeout_seconds,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_step(step: &Step, input: Option<&[u8]>) -> Result<ProcessOutput, String> {
    run_step_in_dir(step, input, None)
}

pub fn run_step_in_dir(
    step: &Step,
    input: Option<&[u8]>,
    working_dir: Option<&Path>,
) -> Result<ProcessOutput, String> {
    let (exit_code, stdout, stderr) = run_captured(step, input, working_dir)?;
    Ok(ProcessOutput {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
    })
}

/// Raw bytes captured by a step, for output that is not text.
pub struct BytesOutput {
    pub exit_code: i32,
    pub bytes: Vec<u8>,
    pub stderr: String,
}

/// Run a step keeping stdout byte-for-byte. `run_step` converts stdout with
/// `String::from_utf8_lossy`, which rewrites every invalid sequence, so a
/// downloaded tarball comes back un-decompressable.
pub fn run_step_bytes(step: &Step, working_dir: Option<&Path>) -> Result<BytesOutput, String> {
    let (exit_code, bytes, stderr) = run_captured(step, None, working_dir)?;
    Ok(BytesOutput {
        exit_code,
        bytes,
        stderr: String::from_utf8_lossy(&stderr).to_string(),
    })
}

fn run_captured(
    step: &Step,
    input: Option<&[u8]>,
    working_dir: Option<&Path>,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let mut cmd = Command::new(&step.program);
    cmd.args(&step.args);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    for name in CONTAMINATING_ENV {
        cmd.env_remove(name);
    }

    if input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn process '{}': {}", step.program, e))?;

    let step_id = NEXT_STEP_ID.fetch_add(1, Ordering::Relaxed);
    let step_started = Instant::now();
    tracker_start(step_id, &step.purpose);

    // stdin is fed on its own thread: a child that writes a full pipe before it
    // reads the rest of its input would otherwise deadlock against us here.
    let stdin_writer = child.stdin.take().map(|mut stdin| {
        let data = input.unwrap_or_default().to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
            // Closing here is what tells a tool like `sudo -S` that no more
            // input is coming.
            drop(stdin);
        })
    });

    // Drain stdout/stderr on dedicated reader threads. Reading only after the
    // child exits deadlocks once a verbose command (e.g. mkarchroot) fills the
    // 64 KiB OS pipe buffer, which previously surfaced as a process timeout.
    let stdout_reader = child
        .stdout
        .take()
        .map(|mut r| std::thread::spawn(move || read_capped(&mut r, MAX_CAPTURED_BYTES)));
    let stderr_reader = child
        .stderr
        .take()
        .map(|mut r| std::thread::spawn(move || read_capped(&mut r, MAX_CAPTURED_BYTES)));

    let start = Instant::now();
    let requested = if step.timeout_seconds == 0 {
        DEFAULT_TIMEOUT_SECONDS
    } else {
        step.timeout_seconds
    };
    let timeout = Duration::from_secs(requested.min(MAX_TIMEOUT_SECONDS));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracker_finish(step_id, &step.purpose, step_started, false);
                    return Err(format!(
                        "Process '{}' timed out after {}s",
                        step.program,
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                // A failed wait leaves the child running; reaping it here is what
                // keeps an RPC daemon from accumulating zombies.
                let _ = child.kill();
                let _ = child.wait();
                tracker_finish(step_id, &step.purpose, step_started, false);
                return Err(format!(
                    "Error waiting for process '{}': {}",
                    step.program, e
                ));
            }
        }
    };

    if let Some(writer) = stdin_writer {
        let _ = writer.join();
    }
    let (stdout_bytes, stdout_problem) = match stdout_reader {
        Some(handle) => handle
            .join()
            .unwrap_or_else(|_| (Vec::new(), Some("the reader failed".to_string()))),
        None => (Vec::new(), None),
    };
    let (stderr_bytes, stderr_problem) = match stderr_reader {
        Some(handle) => handle
            .join()
            .unwrap_or_else(|_| (Vec::new(), Some("the reader failed".to_string()))),
        None => (Vec::new(), None),
    };
    if let Some(problem) = stdout_problem.or(stderr_problem) {
        tracker_finish(step_id, &step.purpose, step_started, false);
        return Err(format!(
            "Output of '{}' could not be captured within {} bytes: {problem}",
            step.program, MAX_CAPTURED_BYTES
        ));
    }

    let exit_ok = status.code().unwrap_or(-1) == 0;
    tracker_finish(step_id, &step.purpose, step_started, exit_ok);

    Ok((status.code().unwrap_or(-1), stdout_bytes, stderr_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capped_read_stops_at_the_budget_instead_of_growing() {
        let payload = vec![7u8; 4000];
        let mut cursor = std::io::Cursor::new(payload.clone());
        let (bytes, problem) = read_capped(&mut cursor, 64 * 1024);
        assert_eq!(bytes, payload, "a stream inside the budget is kept whole");
        assert_eq!(problem, None);

        let mut cursor = std::io::Cursor::new(payload);
        let (bytes, problem) = read_capped(&mut cursor, 1000);
        assert!(bytes.len() <= 1000, "the cap was not honoured");
        let problem = problem.expect("an over-budget stream must be reported");
        assert!(problem.contains("1000 bytes"), "{problem}");
    }

    #[test]
    fn a_stream_exactly_at_the_budget_is_not_truncated() {
        let payload = vec![9u8; 1024];
        let mut cursor = std::io::Cursor::new(payload.clone());
        let (bytes, problem) = read_capped(&mut cursor, 1024);
        assert_eq!(bytes, payload);
        assert_eq!(problem, None);
    }

    #[test]
    fn elapsed_formatting_stays_readable() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(59_999), "59s");
        assert_eq!(format_elapsed(60_000), "1m 00s");
        assert_eq!(format_elapsed(3_725_000), "62m 05s");
    }
}
