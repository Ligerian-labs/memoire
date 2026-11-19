//! Runs the `memoire` binary as a user would, with a private configuration
//! directory and a clean environment.
#![allow(dead_code)]
pub mod fake;

use serde_json::Value;
use std::io::Write as _;
use std::process::{Command, Stdio};

pub struct Run {
    pub args: Vec<String>,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// Standard output as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!("stdout is not JSON ({e}): {}\n{}", self.stdout, self.stderr)
        })
    }

    /// Standard error as the JSON error envelope.
    pub fn error(&self) -> Value {
        serde_json::from_str(self.stderr.trim())
            .unwrap_or_else(|e| panic!("stderr is not JSON ({e}): {}", self.stderr))
    }

    pub fn ok(self) -> Self {
        assert_eq!(
            self.code, 0,
            "memoire {:?}\nstdout: {}\nstderr: {}",
            self.args, self.stdout, self.stderr
        );
        self
    }
}

pub struct Memoire {
    pub config: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl Memoire {
    pub fn new() -> Self {
        Self {
            config: tempfile::tempdir().unwrap(),
            env: Vec::new(),
        }
    }

    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    pub fn credentials(&self) -> std::path::PathBuf {
        self.config.path().join("memoire/credentials.json")
    }

    /// `memoire ARGS` with `stdin` piped in.
    pub fn run(&self, args: &[&str], stdin: &str) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_memoire"));
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.config.path())
            .env("MEMOIRE_CONFIG_DIR", self.config.path().join("memoire"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in &self.env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let mut pipe = child.stdin.take().unwrap();
        let input = stdin.to_owned();
        let writer = std::thread::spawn(move || {
            let _ = pipe.write_all(input.as_bytes());
        });
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap();
        Run {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}
