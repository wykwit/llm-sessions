//! Insert mode: hand one user message to an agent's own headless CLI.
//!
//! The agent writes the turn into its store itself; `llm-sessions` never
//! touches the session file. The command runs off the UI thread with stdin
//! closed and output captured, so the TUI keeps drawing and the terminal is
//! never shared with the child. It is fire-and-report: nothing supervises or
//! kills the agent once it has started.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// A message on its way. Poll it until it reports an outcome.
pub struct Delivery {
    /// What the status line calls the target, e.g. `omp 01a0-7b22`.
    pub label: String,
    outcome: Receiver<Result<(), String>>,
}

impl Delivery {
    /// `Some` once the agent has exited: `Ok` on a zero status, otherwise
    /// the reason in one line.
    pub fn poll(&self) -> Option<Result<(), String>> {
        match self.outcome.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("delivery thread vanished".into())),
        }
    }
}

/// Run `command` in `cwd` on a background thread.
pub fn send(label: String, command: Vec<String>, cwd: Option<PathBuf>) -> Delivery {
    let (tx, outcome) = mpsc::channel();
    std::thread::spawn(move || {
        // A closed receiver means the TUI quit; the agent still ran.
        let _ = tx.send(run(&command, cwd));
    });
    Delivery { label, outcome }
}

fn run(command: &[String], cwd: Option<PathBuf>) -> Result<(), String> {
    let (program, args) = command.split_first().ok_or("empty command")?;
    let mut child = Command::new(program);
    child
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    let output = child
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    // The last non-empty stderr line is where CLIs put the reason; stdout is
    // the fallback for tools that report errors there.
    let reason = [&output.stderr, &output.stdout]
        .into_iter()
        .find_map(|stream| {
            String::from_utf8_lossy(stream)
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| output.status.to_string());
    Err(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(delivery: &Delivery) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(outcome) = delivery.poll() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "delivery never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn a_failing_agent_reports_its_last_stderr_line() {
        let delivery = send(
            "x".into(),
            sh("echo noise >&2; echo 'session not found' >&2; exit 3"),
            None,
        );
        assert_eq!(wait(&delivery), Err("session not found".into()));
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_hang() {
        let delivery = send("x".into(), vec!["llm-sessions-no-such-agent".into()], None);
        assert!(
            wait(&delivery)
                .unwrap_err()
                .contains("llm-sessions-no-such-agent")
        );
    }
}
