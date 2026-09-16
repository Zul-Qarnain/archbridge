use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    let mut cmd = Command::new(&step.program);
    cmd.args(&step.args);

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

    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data);
        }
    }

    let start = Instant::now();
    let timeout = Duration::from_secs(if step.timeout_seconds == 0 {
        30
    } else {
        step.timeout_seconds
    });

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout_bytes = child.stdout.take().map_or(vec![], |mut r| {
                    let mut buf = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut r, &mut buf);
                    buf
                });
                let stderr_bytes = child.stderr.take().map_or(vec![], |mut r| {
                    let mut buf = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut r, &mut buf);
                    buf
                });

                return Ok(ProcessOutput {
                    exit_code: status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
                    stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "Process '{}' timed out after {}s",
                        step.program,
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(format!(
                    "Error waiting for process '{}': {}",
                    step.program, e
                ))
            }
        }
    }
}
