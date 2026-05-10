// Shared fixtures for integration tests.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

pub const BIN: &str = env!("CARGO_BIN_EXE_diffview");

pub struct Repo {
    pub path: PathBuf,
    _tmp: TempDir,
}

impl Repo {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("repo");
        fs::create_dir(&path).unwrap();
        let init_cmds: &[&[&str]] = &[
            &["init", "-q", "-b", "main"],
            &["config", "user.email", "t@e"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
        ];
        for cmd in init_cmds {
            let ok = Command::new("git")
                .args(*cmd)
                .current_dir(&path)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git init step failed: {:?}", cmd);
        }
        fs::write(path.join("init.txt"), "seed\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&path)
            .status()
            .unwrap();
        Self { path, _tmp: tmp }
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(p, content).unwrap();
    }

    pub fn commit_all(&self, msg: &str) -> String {
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", msg])
            .current_dir(&self.path)
            .status()
            .unwrap();
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    pub fn hash_object(&self, rel: &str) -> String {
        self.git(&["hash-object", rel]).trim().to_string()
    }
}

pub struct State {
    pub env: HashMap<String, String>,
    pub dir: PathBuf,
    _tmp: TempDir,
}

impl State {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        Self {
            env: HashMap::new(),
            dir,
            _tmp: tmp,
        }
    }

    pub fn file(&mut self, var: &str, content: &str) -> PathBuf {
        let p = self.dir.join(var);
        fs::write(&p, content).unwrap();
        self.env
            .insert(var.to_string(), p.to_string_lossy().into_owned());
        p
    }

    pub fn setenv(&mut self, var: &str, val: &str) {
        self.env.insert(var.to_string(), val.to_string());
    }
}

pub struct Stubs {
    pub dir: PathBuf,
    pub logdir: PathBuf,
    _tmp: TempDir,
}

impl Stubs {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let logdir = dir.join("_logs");
        fs::create_dir(&logdir).unwrap();
        Self {
            dir,
            logdir,
            _tmp: tmp,
        }
    }

    pub fn add(&self, name: &str, body: &str) {
        let p = self.dir.join(name);
        fs::write(&p, format!("#!/bin/bash\n{}\n", body)).unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).unwrap();
    }

    pub fn record(&self, name: &str, prelude: &str) {
        let log = self.logdir.join(format!("{}.log", name));
        let log_str = log.display().to_string();
        let body = format!(
            "{prelude}\nprintf '%s\\t' \"$@\" >> {log}\nprintf '\\n' >> {log}\nexit 0",
            prelude = prelude,
            log = log_str,
        );
        self.add(name, &body);
    }

    pub fn calls(&self, name: &str) -> Vec<Vec<String>> {
        let log = self.logdir.join(format!("{}.log", name));
        let content = match fs::read_to_string(&log) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for line in content.lines() {
            let line = line.strip_suffix('\t').unwrap_or(line);
            rows.push(line.split('\t').map(String::from).collect());
        }
        rows
    }
}

pub struct RunResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

pub fn run(repo: &Repo, state: &State, stubs: &Stubs, args: &[&str]) -> RunResult {
    run_with(repo, state, stubs, args, &[])
}

pub fn run_with(
    repo: &Repo,
    state: &State,
    stubs: &Stubs,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> RunResult {
    let mut cmd = Command::new(BIN);
    cmd.args(args).current_dir(&repo.path);

    // Strip any DIFFVIEW_* from inherited env.
    for (k, _) in std::env::vars() {
        if k.starts_with("DIFFVIEW_") {
            cmd.env_remove(&k);
        }
    }
    let host_path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("{}:{}", stubs.dir.display(), host_path));
    for (k, v) in &state.env {
        cmd.env(k, v);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out: Output = cmd.output().unwrap();
    RunResult {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

pub fn parse_list_rows(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            line.split_once('\x1f')
                .map(|(d, t)| (d.to_string(), t.to_string()))
        })
        .collect()
}

/// Strip ANSI escape sequences (delta colorizes preview output).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // skip until alphabetic
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}
