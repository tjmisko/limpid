//! `:!cmd`: run a command in the user's shell and collect its output.

use std::{
    ffi::OsStr,
    process::{Command, Stdio},
    sync::mpsc::{Receiver, TryRecvError, channel},
    thread,
};

/// One finished command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub command: String,
    pub output: String,
    pub success: bool,
}

/// Commands started by `:!`, with the output of those that have finished.
#[derive(Default)]
pub struct Shell {
    runs: Vec<Receiver<Run>>,
}

impl Shell {
    pub fn start(&mut self, command: &str) {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "sh".into());
        self.start_with(&shell, command);
    }

    fn start_with(&mut self, shell: &OsStr, command: &str) {
        let (sender, receiver) = channel();
        let command = command.to_owned();
        let shell = shell.to_owned();
        thread::spawn(move || {
            let _ = sender.send(run(&shell, &command));
        });
        self.runs.push(receiver);
    }

    pub fn busy(&self) -> bool {
        !self.runs.is_empty()
    }

    /// The commands that finished since the last call.
    pub fn poll(&mut self) -> Vec<Run> {
        let mut done = Vec::new();
        self.runs.retain(|receiver| match receiver.try_recv() {
            Ok(run) => {
                done.push(run);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        done
    }
}

fn run(shell: &OsStr, command: &str) -> Run {
    match Command::new(shell)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let errors = String::from_utf8_lossy(&output.stderr);
            if !errors.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&errors);
            }
            Run {
                command: command.to_owned(),
                output: text,
                success: output.status.success(),
            }
        }
        Err(error) => Run {
            command: command.to_owned(),
            output: format!("{}: {error}", shell.to_string_lossy()),
            success: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn finish(shell: &mut Shell) -> Run {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(run) = shell.poll().into_iter().next() {
                return run;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("the command did not finish");
    }

    fn sh() -> &'static OsStr {
        OsStr::new("/bin/sh")
    }

    #[test]
    fn should_capture_stdout_and_stderr_when_the_command_writes_both() {
        let mut shell = Shell::default();
        shell.start_with(sh(), "echo out; echo err >&2");
        assert!(shell.busy());
        let run = finish(&mut shell);
        assert!(run.success);
        assert!(run.output.contains("out"));
        assert!(run.output.contains("err"));
        assert!(!shell.busy());
    }

    #[test]
    fn should_report_failure_when_the_command_exits_non_zero() {
        let mut shell = Shell::default();
        shell.start_with(sh(), "echo bad >&2; exit 3");
        let run = finish(&mut shell);
        assert!(!run.success);
        assert!(run.output.contains("bad"));
    }
}
