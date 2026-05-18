// fzf-driven diff viewer.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_CONTEXT: u32 = 4;
const MAX_FILES: usize = 500;
const UNPREFIXED_KEYS: &str = "w,x,s,g,o,q,/";
const IGNORE_KEYS: &str = "a,b,c,d,e,f,h,i,j,k,l,m,n,p,r,t,u,v,y,z,\
A,B,C,D,E,F,G,H,I,J,K,L,M,N,O,P,Q,R,S,T,U,V,W,X,Y,Z,\
0,1,2,3,4,5,6,7,8,9,space";
const VIEW_GHOST: &str = "/ to filter";
const SHORTEN_KEEP: usize = 0;

fn all_typing_keys() -> String {
    format!("{},{}", UNPREFIXED_KEYS, IGNORE_KEYS)
}

const SUBCOMMANDS: &[&str] = &[
    "list", "toggle", "resize", "send", "enter-action", "escape-action",
    "context", "open", "toggle-ws", "toggle-exclude", "toggle-sbs",
    "toggle-target", "enter-filter", "preview", "header",
];

// --- subprocess helpers ---

fn git(args: &[&str]) -> String {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<String>, String>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let key: Vec<String> = args.iter().map(|&s| s.to_string()).collect();
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return v.clone();
    }
    let result = match Command::new("git").args(args).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    };
    cache.lock().unwrap().insert(key, result.clone());
    result
}

fn git_ok(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn read_file(path: &str, default: &str) -> String {
    if path.is_empty() {
        return default.to_string();
    }
    match fs::read_to_string(path) {
        Ok(s) => s.trim_end_matches('\n').to_string(),
        Err(_) => default.to_string(),
    }
}

fn env_var(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

fn write_file(path: &str, content: &str) {
    if path.is_empty() {
        return;
    }
    let _ = fs::write(path, content);
}

fn current_self() -> String {
    env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "diffview".to_string())
}

/// Compute git's blob SHA-1 in-process — equivalent to `git hash-object <path>`.
fn hash_blob(path: &Path) -> Option<String> {
    use sha1::{Digest, Sha1};
    let bytes = fs::read(path).ok()?;
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}", bytes.len()).as_bytes());
    hasher.update(b"\0");
    hasher.update(&bytes);
    Some(format!("{:x}", hasher.finalize()))
}

// --- args ---

#[derive(Default, Debug)]
struct Args {
    ws: String,
    merge_base: bool,
    upstream: bool,
    commit_back: String,
    exclude: bool,
    sidebyside: bool,
    target_mode: bool,
    subcmd: String,
    subcmd_args: Vec<String>,
}

fn parse_args(argv: &[String]) -> Args {
    let mut a = Args::default();
    for arg in argv {
        if !a.subcmd.is_empty() {
            a.subcmd_args.push(arg.clone());
            continue;
        }
        if let Some(stripped) = arg.strip_prefix('~') {
            if !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit()) {
                a.commit_back = stripped.to_string();
                continue;
            }
        }
        match arg.as_str() {
            "w" => a.ws = "-w".to_string(),
            "u" => {}
            "m" => a.merge_base = true,
            "p" => a.upstream = true,
            "x" => a.exclude = true,
            "s" => a.sidebyside = true,
            "t" => a.target_mode = true,
            s if SUBCOMMANDS.contains(&s) => a.subcmd = s.to_string(),
            _ => {}
        }
    }
    a
}

// --- initial state (env vars) ---

fn init_state(a: &mut Args) {
    if env::var("DIFFVIEW_TARGET").is_ok() {
        return;
    }
    let head_target = "HEAD";

    // Independent calls fire in background while the verify loop + merge-base run sequentially.
    let h_git_dir = std::thread::spawn(|| git(&["rev-parse", "--git-dir"]));
    let h_toplevel = std::thread::spawn(|| git(&["rev-parse", "--show-toplevel"]));

    let mut base_branch = "origin/main".to_string();
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if git_ok(&["rev-parse", "--verify", cand]) {
            base_branch = cand.to_string();
            break;
        }
    }
    let mb = git(&["merge-base", "HEAD", &base_branch]).trim().to_string();
    let merge_base_target = if mb.is_empty() { "HEAD".to_string() } else { mb };

    let upstream_target = git(&["rev-parse", "--verify", "--quiet", "@{upstream}"])
        .trim()
        .to_string();
    let upstream_name = if upstream_target.is_empty() {
        String::new()
    } else {
        git(&["rev-parse", "--abbrev-ref", "@{upstream}"]).trim().to_string()
    };

    if a.upstream && upstream_target.is_empty() {
        eprintln!("diffview: 'p' mode needs an upstream — none configured, falling back to target picker");
        a.upstream = false;
        a.target_mode = true;
    }

    let diff_target = if !a.commit_back.is_empty() {
        format!("HEAD~{}^..HEAD~{}", a.commit_back, a.commit_back)
    } else if a.upstream {
        upstream_target.clone()
    } else if a.merge_base {
        merge_base_target.clone()
    } else {
        head_target.to_string()
    };

    let git_dir = h_git_dir.join().unwrap_or_default();
    let toplevel = h_toplevel.join().unwrap_or_default();

    unsafe {
        env::set_var("DIFFVIEW_TARGET", &diff_target);
        env::set_var("DIFFVIEW_HEAD_TARGET", head_target);
        env::set_var("DIFFVIEW_MERGE_BASE_TARGET", &merge_base_target);
        env::set_var("DIFFVIEW_UPSTREAM_TARGET", &upstream_target);
        env::set_var("DIFFVIEW_UPSTREAM_NAME", &upstream_name);
        env::set_var("DIFFVIEW_WS", &a.ws);
        env::set_var("DIFFVIEW_EXCLUDE", if a.exclude { "true" } else { "false" });
        env::set_var("DIFFVIEW_VIEWED", format!("{}/diff-viewed", git_dir.trim()));
        env::set_var("DIFFVIEW_TOPLEVEL", toplevel.trim());
    }
}

// --- helpers ---

fn format_stats(shortstat: &str) -> String {
    if shortstat.is_empty() {
        return String::new();
    }
    let ins = capture_int(shortstat, " insertion").unwrap_or(0);
    let dele = capture_int(shortstat, " deletion").unwrap_or(0);
    if ins == 0 && dele == 0 {
        return String::new();
    }
    format!("+{}/-{}", ins, dele)
}

fn capture_int(haystack: &str, suffix: &str) -> Option<u64> {
    let idx = haystack.find(suffix)?;
    let prefix = &haystack[..idx];
    let digits: String = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

fn diff_stats(ws: &str, target: &str) -> String {
    let mut ins: u64 = 0;
    let mut dele: u64 = 0;
    let mut args: Vec<&str> = vec!["diff", "--numstat"];
    if !ws.is_empty() {
        args.push(ws);
    }
    args.push(target);
    for line in git(&args).lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        if parts[0] == "-" {
            continue;
        }
        if let (Ok(i), Ok(d)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
            ins += i;
            dele += d;
        }
    }
    if !target.contains("..") {
        let toplevel = env_var("DIFFVIEW_TOPLEVEL", "");
        for f in git(&["ls-files", "--others", "--exclude-standard"]).lines() {
            if f.is_empty() {
                continue;
            }
            let p = Path::new(&toplevel).join(f);
            if !p.is_file() {
                continue;
            }
            if let Ok(bytes) = fs::read(&p) {
                let n = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
                if n > 0 {
                    ins += n;
                }
            }
        }
    }
    if ins == 0 && dele == 0 {
        String::new()
    } else {
        format!("+{}/-{}", ins, dele)
    }
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let keep = SHORTEN_KEEP + 1;
    if parts.len() <= keep {
        return path.to_string();
    }
    let head_count = parts.len() - keep;
    let mut head = String::new();
    for p in &parts[..head_count] {
        let prefix: String = p.chars().take(2).collect();
        head.push_str(&prefix);
        head.push('/');
    }
    head.push_str(&parts[head_count..].join("/"));
    head
}

// --- header ---

fn header() -> String {
    let target = read_file(&env_var("DIFFVIEW_TARGET_FILE", ""), &env_var("DIFFVIEW_TARGET", ""));
    let mode = read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "");
    let mut parts: Vec<&str> = Vec::new();
    if read_file(&env_var("DIFFVIEW_SBS_FILE", ""), "") == "true" {
        parts.push("s");
    }
    if read_file(&env_var("DIFFVIEW_WS_FILE", ""), "") == "-w" {
        parts.push("w");
    }
    if read_file(&env_var("DIFFVIEW_EXCLUDE_FILE", ""), "") == "true" {
        parts.push("x");
    }
    let mut target_part = String::new();
    if mode != "target" {
        let mb = env_var("DIFFVIEW_MERGE_BASE_TARGET", "");
        let up = env_var("DIFFVIEW_UPSTREAM_TARGET", "");
        if target == "HEAD" {
            target_part = "u".to_string();
        } else if !up.is_empty() && target == up {
            target_part = "p".to_string();
        } else if target == mb && target != "HEAD" {
            target_part = "m".to_string();
        } else if target.contains("^..") {
            let tip = target.splitn(2, "..").nth(1).unwrap_or("");
            let n = git(&["rev-list", "--count", &format!("{}..HEAD", tip)])
                .trim()
                .to_string();
            target_part = format!("~{}", if n.is_empty() { "?".to_string() } else { n });
        }
    }
    let flags = parts.join(" ");
    let ws = read_file(&env_var("DIFFVIEW_WS_FILE", ""), "");
    let stats = diff_stats(&ws, &target);
    let rhs = if !target_part.is_empty() && !stats.is_empty() {
        format!("{} {}", target_part, stats)
    } else if !target_part.is_empty() {
        target_part
    } else {
        stats
    };
    if !flags.is_empty() && !rhs.is_empty() {
        format!("{} · {}", flags, rhs)
    } else if !rhs.is_empty() {
        rhs
    } else {
        flags
    }
}

// --- list generators ---

/// Parse a rename path from `git diff --numstat`.
///
/// Two formats: plain `old => new`, or brace shorthand `prefix{old => new}suffix`
/// when the renamed paths share a leading and/or trailing component (git collapses
/// the unchanged parts). Returns `(old, new)` or `None` if the path has no rename arrow.
fn parse_rename_path(path: &str) -> Option<(String, String)> {
    let arrow = path.find(" => ")?;
    if let Some(brace_open) = path[..arrow].rfind('{') {
        if let Some(rel_brace_close) = path[arrow..].find('}') {
            let brace_close = arrow + rel_brace_close;
            let prefix = &path[..brace_open];
            let old_part = &path[brace_open + 1..arrow];
            let new_part = &path[arrow + 4..brace_close];
            let suffix = &path[brace_close + 1..];
            // When one side is empty, the surrounding slashes collapse: `a/{x => }/b` → new=`a/b`.
            let assemble = |part: &str| -> String {
                if part.is_empty() && prefix.ends_with('/') && suffix.starts_with('/') {
                    format!("{}{}", prefix, &suffix[1..])
                } else {
                    format!("{}{}{}", prefix, part, suffix)
                }
            };
            return Some((assemble(old_part), assemble(new_part)));
        }
    }
    Some((path[..arrow].to_string(), path[arrow + 4..].to_string()))
}

/// Parse `git diff --numstat`. Returns (paths-in-order, file_stats, renamed).
/// `paths` contains every changed file (incl. binaries, incl. 0/0 renames) — lets
/// callers skip a redundant `git diff --name-only` against the same target.
fn parse_numstat(
    _ws: &str,
    target: &str,
) -> (Vec<String>, HashMap<String, String>, HashMap<String, String>) {
    let mut paths: Vec<String> = Vec::new();
    let mut file_stats: HashMap<String, String> = HashMap::new();
    let mut renamed: HashMap<String, String> = HashMap::new();
    let args: Vec<&str> = vec!["diff", "--numstat", target];
    for line in git(&args).lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let (ins_s, del_s, mut path) = (parts[0], parts[1], parts[2].to_string());
        if path.is_empty() {
            continue;
        }
        if let Some((old, new)) = parse_rename_path(&path) {
            renamed.insert(new.clone(), old);
            path = new;
        }
        paths.push(path.clone());
        if ins_s == "-" {
            // binary file — listed but no line stats
            continue;
        }
        if let (Ok(i), Ok(d)) = (ins_s.parse::<u64>(), del_s.parse::<u64>()) {
            if i > 0 || d > 0 {
                file_stats.insert(path, format!("+{}/-{}", i, d));
            }
        }
    }
    (paths, file_stats, renamed)
}

fn load_viewed() -> HashMap<String, String> {
    let mut viewed: HashMap<String, String> = HashMap::new();
    let p = env::var("DIFFVIEW_VIEWED").unwrap_or_default();
    if p.is_empty() || !Path::new(&p).is_file() {
        return viewed;
    }
    let content = match fs::read_to_string(&p) {
        Ok(c) => c,
        Err(_) => return viewed,
    };
    for line in content.lines() {
        if let Some(idx) = line.find(' ') {
            let h = &line[..idx];
            let path = &line[idx + 1..];
            if !h.is_empty() && !path.is_empty() {
                viewed.insert(path.to_string(), h.to_string());
            }
        }
    }
    viewed
}

fn generate_list() -> String {
    let target = read_file(
        &env_var("DIFFVIEW_TARGET_FILE", ""),
        &env_var("DIFFVIEW_TARGET", ""),
    );
    let exclude_val = read_file(
        &env_var("DIFFVIEW_EXCLUDE_FILE", ""),
        &env_var("DIFFVIEW_EXCLUDE", ""),
    );
    let ws = read_file(&env_var("DIFFVIEW_WS_FILE", ""), "");
    let toplevel = env_var("DIFFVIEW_TOPLEVEL", "");

    let (tracked, file_stats, renamed) = parse_numstat(&ws, &target);
    let is_range = target.contains("..");
    let untracked: Vec<String> = if is_range {
        Vec::new()
    } else {
        git(&["ls-files", "--others", "--exclude-standard"])
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    };
    let total_count = tracked.len() + untracked.len();
    let truncated = total_count > MAX_FILES;

    let mut all_files: Vec<String> = if truncated && tracked.len() < MAX_FILES {
        let remaining = MAX_FILES - tracked.len();
        tracked.into_iter().chain(untracked.into_iter().take(remaining)).collect()
    } else {
        tracked.into_iter().chain(untracked).collect()
    };
    all_files.sort();
    all_files.dedup();
    if all_files.len() > MAX_FILES {
        all_files.truncate(MAX_FILES);
    }

    if exclude_val == "true" {
        all_files.retain(|f| !f.contains("/generated/"));
    }

    let viewed = load_viewed();

    let mut lines: Vec<String> = Vec::with_capacity(all_files.len() + if truncated { 1 } else { 0 });
    for f in &all_files {
        let short = shorten_path(f);
        let mut stats = file_stats.get(f).cloned().unwrap_or_default();
        if stats.is_empty() && !renamed.contains_key(f) && !is_range {
            let p = Path::new(&toplevel).join(f);
            if p.is_file() {
                if let Ok(bytes) = fs::read(&p) {
                    let n = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
                    if n > 0 {
                        stats = format!("+{}/-0", n);
                    }
                }
            }
        }
        let suffix = if !stats.is_empty() {
            format!(" ({})", stats)
        } else if renamed.contains_key(f) {
            " (rename)".to_string()
        } else {
            String::new()
        };
        let stored = viewed.get(f).cloned().unwrap_or_default();
        let mut marker = "  ";
        if !stored.is_empty() {
            let p = Path::new(&toplevel).join(f);
            if p.is_file() {
                if let Some(cur) = hash_blob(&p) {
                    if cur == stored {
                        marker = "✓ ";
                    }
                }
            } else if stored == "DELETED" {
                marker = "✓ ";
            }
        }
        lines.push(format!("{}{}{}\x1f{}", marker, short, suffix, f));
    }
    if truncated {
        lines.insert(0, format!(
            "\x1b[2m* Showing {} of {} files\x1b[0m\x1f__TOO_MANY__",
            all_files.len(), total_count
        ));
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn emit_commits_shortstat(ws: &str, log_args: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["log", "--shortstat", "--pretty=tformat:COMMIT %h\t%P\t%s"];
    if !ws.is_empty() {
        args.push(ws);
    }
    args.extend_from_slice(log_args);
    let out = git(&args);
    let mut lines: Vec<String> = Vec::new();
    let mut commit_line = String::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("COMMIT ") {
            commit_line = rest.to_string();
        } else if !line.is_empty() && !commit_line.is_empty() {
            let sp: Vec<&str> = commit_line.splitn(3, '\t').collect();
            if sp.len() != 3 {
                commit_line.clear();
                continue;
            }
            let (h, parents, subj) = (sp[0], sp[1], sp[2]);
            // Root commits (no parent) can't use `<h>^..<h>` — substitute empty tree.
            let base = if parents.is_empty() {
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
            } else {
                format!("{}^", h)
            };
            let stat = format_stats(line);
            if !stat.is_empty() {
                lines.push(format!("{} ({}) {}\x1f{}..{}", subj, stat, h, base, h));
            } else {
                lines.push(format!("{} {}\x1f{}..{}", subj, h, base, h));
            }
            commit_line.clear();
        }
    }
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

fn upstream_row(ws: &str, mb: &str) -> Option<String> {
    let upstream = env_var("DIFFVIEW_UPSTREAM_TARGET", "");
    if upstream.is_empty() || upstream == mb {
        return None;
    }
    let upstream_name = env_var("DIFFVIEW_UPSTREAM_NAME", "");
    let up_short = git(&["rev-parse", "--short", &upstream]).trim().to_string();
    let up_stats = diff_stats(ws, &upstream);
    let label: &str = if upstream_name.is_empty() { "upstream" } else { &upstream_name };
    Some(if !up_stats.is_empty() {
        format!("* Vs {} ({}) {}\x1f{}", label, up_stats, up_short, upstream)
    } else {
        format!("* Vs {} {}\x1f{}", label, up_short, upstream)
    })
}

fn generate_target_list() -> String {
    let ws = read_file(&env_var("DIFFVIEW_WS_FILE", ""), "");
    let limit_s = read_file(&env_var("DIFFVIEW_RECENT_LIMIT_FILE", ""), "");
    let limit: u32 = if limit_s.is_empty() {
        10
    } else {
        limit_s.parse().unwrap_or(10)
    };
    let mut out: Vec<String> = Vec::new();
    let stats_u = diff_stats(&ws, "HEAD");
    if !stats_u.is_empty() {
        out.push(format!("* Uncommitted ({})\x1fHEAD", stats_u));
    } else {
        out.push("* Uncommitted\x1fHEAD".to_string());
    }
    let mb = env_var("DIFFVIEW_MERGE_BASE_TARGET", "");
    if mb != "HEAD" {
        let ec = emit_commits_shortstat(&ws, &[&format!("{}..HEAD", mb)]);
        let ec = ec.trim_end_matches('\n');
        if !ec.is_empty() {
            out.push(ec.to_string());
        }
        let short = git(&["rev-parse", "--short", &mb]).trim().to_string();
        let stats_fp = diff_stats(&ws, &mb);
        if !stats_fp.is_empty() {
            out.push(format!("* Since merge base ({}) {}\x1f{}", stats_fp, short, mb));
        } else {
            out.push(format!("* Since merge base {}\x1f{}", short, mb));
        }
        if let Some(row) = upstream_row(&ws, &mb) {
            out.push(row);
        }
        let limit_str = limit.to_string();
        let ec = emit_commits_shortstat(&ws, &["-n", &limit_str, &mb]);
        let ec = ec.trim_end_matches('\n');
        if !ec.is_empty() {
            out.push(ec.to_string());
        }
        if git_ok(&["rev-parse", "--quiet", "--verify", &format!("{}~{}", mb, limit)]) {
            out.push("* Load 10 more\x1f__LOAD_MORE__".to_string());
        }
    } else {
        if let Some(row) = upstream_row(&ws, &mb) {
            out.push(row);
        }
        let limit_str = limit.to_string();
        let ec = emit_commits_shortstat(&ws, &["-n", &limit_str, "HEAD"]);
        let ec = ec.trim_end_matches('\n');
        if !ec.is_empty() {
            out.push(ec.to_string());
        }
        if git_ok(&["rev-parse", "--quiet", "--verify", &format!("HEAD~{}", limit)]) {
            out.push("* Load 10 more\x1f__LOAD_MORE__".to_string());
        }
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

// --- subcommand dispatch ---

fn cmd_list() -> i32 {
    let mode = read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "");
    let s = if mode == "target" {
        generate_target_list()
    } else {
        generate_list()
    };
    let _ = std::io::stdout().write_all(s.as_bytes());
    0
}

fn cmd_header() -> i32 {
    println!("{}", header());
    0
}

fn toggle_viewed(file: &str) {
    if file.is_empty() {
        return;
    }
    let toplevel = env_var("DIFFVIEW_TOPLEVEL", "");
    if env::set_current_dir(&toplevel).is_err() {
        return;
    }
    let p = Path::new(file);
    let cur = if p.is_file() {
        hash_blob(p).unwrap_or_default()
    } else {
        "DELETED".to_string()
    };
    if cur.is_empty() {
        return;
    }
    let viewed_path = env_var("DIFFVIEW_VIEWED", "");
    let suffix = format!(" {}", file);
    let existing = fs::read_to_string(&viewed_path).unwrap_or_default();
    let mut stored = String::new();
    let mut other_lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        if line.ends_with(&suffix) {
            if stored.is_empty() {
                if let Some(idx) = line.find(' ') {
                    stored = line[..idx].to_string();
                }
            }
        } else {
            other_lines.push(line.to_string());
        }
    }
    let new_content = if stored == cur {
        if other_lines.is_empty() {
            String::new()
        } else {
            let mut s = other_lines.join("\n");
            s.push('\n');
            s
        }
    } else {
        other_lines.push(format!("{} {}", cur, file));
        let mut s = other_lines.join("\n");
        s.push('\n');
        s
    };
    let _ = fs::write(&viewed_path, new_content);
}

fn cmd_toggle(args: &[String]) -> i32 {
    if read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "") == "target" {
        return 0;
    }
    toggle_viewed(args.first().map(|s| s.as_str()).unwrap_or(""));
    0
}

fn cmd_toggle_ws() -> i32 {
    let path = env_var("DIFFVIEW_WS_FILE", "");
    let cur = read_file(&path, "");
    let new = if cur == "-w" { "" } else { "-w" };
    write_file(&path, new);
    println!(
        "reload-sync({} list)+refresh-preview+change-header({})",
        current_self(),
        header()
    );
    0
}

fn cmd_toggle_sbs() -> i32 {
    let path = env_var("DIFFVIEW_SBS_FILE", "");
    let cur = read_file(&path, "");
    let new = if cur == "true" { "false" } else { "true" };
    write_file(&path, new);
    println!(
        "reload-sync({} list)+refresh-preview+change-header({})",
        current_self(),
        header()
    );
    0
}

fn cmd_toggle_exclude() -> i32 {
    let path = env_var("DIFFVIEW_EXCLUDE_FILE", "");
    let cur = read_file(&path, "");
    let new = if cur == "true" { "false" } else { "true" };
    write_file(&path, new);
    println!(
        "reload-sync({} list)+change-header({})",
        current_self(),
        header()
    );
    0
}

fn cmd_toggle_target(args: &[String]) -> i32 {
    let prompt = args.first().cloned().unwrap_or_default();
    let query = args.get(1).cloned().unwrap_or_default();
    if prompt.starts_with("send") {
        return 0;
    }
    if read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "") == "target" {
        return 0;
    }
    write_file(&env_var("DIFFVIEW_FILTER_FILE", ""), &query);
    write_file(&env_var("DIFFVIEW_MODE_FILE", ""), "target");
    println!(
        "reload-sync({0} list)+disable-search+rebind({1})\
+change-prompt(target> )+clear-query+refresh-preview+first\
+change-header({2})+change-ghost({3})",
        current_self(),
        all_typing_keys(),
        header(),
        VIEW_GHOST,
    );
    0
}

fn cmd_enter_filter() -> i32 {
    println!(
        "enable-search+unbind({})+change-prompt(filter> )+change-ghost()",
        all_typing_keys()
    );
    0
}

fn cmd_resize(args: &[String]) -> i32 {
    let delta_s = args.first().cloned().unwrap_or_else(|| "0".to_string());
    let prompt = args.get(1).cloned().unwrap_or_default();
    if prompt.starts_with("filter") || prompt.starts_with("send") {
        if delta_s.starts_with('-') {
            println!("forward-word");
        } else {
            println!("backward-word");
        }
        return 0;
    }
    let path = env_var("DIFFVIEW_SIZE_FILE", "");
    let cur_s = read_file(&path, "");
    let cur: i32 = if cur_s.is_empty() { 80 } else { cur_s.parse().unwrap_or(80) };
    let delta: i32 = delta_s.parse().unwrap_or(0);
    let new = (cur + delta).clamp(20, 95);
    write_file(&path, &new.to_string());
    println!("change-preview-window(right,{}%,wrap)", new);
    0
}

fn cmd_send() -> i32 {
    let file = read_file(&env_var("DIFFVIEW_SEND_FILE", ""), "");
    let query = read_file(&env_var("DIFFVIEW_QUERY_FILE", ""), "");
    let mut tmux_env: HashMap<String, String> = env::vars().collect();
    tmux_env.remove("TMUX");
    let caller_out = match Command::new("tmux")
        .args(["show-environment", "-g", "DIFFVIEW_CALLER"])
        .env_clear()
        .envs(&tmux_env)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    };
    let caller = caller_out.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default();
    if !query.is_empty() && !caller.is_empty() {
        let msg = if !file.is_empty() {
            format!("In {}: {}", file, query)
        } else {
            query.clone()
        };
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &caller, "-l", &msg])
            .env_clear()
            .envs(&tmux_env)
            .status();
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &caller, "Enter"])
            .env_clear()
            .envs(&tmux_env)
            .status();
    }
    0
}

fn cmd_context(args: &[String]) -> i32 {
    let path = env_var("DIFFVIEW_CONTEXT_FILE", "");
    let cur_s = read_file(&path, "");
    let cur: i32 = if cur_s.is_empty() { 7 } else { cur_s.parse().unwrap_or(7) };
    let delta: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let new = (cur + delta).clamp(0, 999);
    write_file(&path, &new.to_string());
    0
}

fn cmd_open(args: &[String]) -> ! {
    if read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "") == "target" {
        std::process::exit(0);
    }
    let file = args.first().cloned().unwrap_or_default();
    let toplevel = env_var("DIFFVIEW_TOPLEVEL", "");
    let _ = env::set_current_dir(&toplevel);
    let ws_val = read_file(&env_var("DIFFVIEW_WS_FILE", ""), &env_var("DIFFVIEW_WS", ""));
    let target = read_file(&env_var("DIFFVIEW_TARGET_FILE", ""), &env_var("DIFFVIEW_TARGET", ""));
    let mut diff_args: Vec<&str> = vec!["diff"];
    if !ws_val.is_empty() {
        diff_args.push(&ws_val);
    }
    diff_args.extend_from_slice(&[&target, "--", &file]);
    let mut line = "1".to_string();
    for ln in git(&diff_args).lines() {
        if ln.starts_with("@@") {
            // extract first +N from the hunk header
            if let Some(idx) = ln.find('+') {
                let rest = &ln[idx + 1..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    line = digits;
                }
            }
            break;
        }
    }
    let rc = Command::new("nvim")
        .arg(format!("+{}", line))
        .arg(&file)
        .status()
        .map(|s| s.code().unwrap_or(0))
        .unwrap_or_else(|e| {
            eprintln!("diffview: failed to spawn nvim: {}", e);
            127
        });
    std::process::exit(rc);
}

fn cmd_enter_action(args: &[String]) -> i32 {
    let prompt = args.first().cloned().unwrap_or_default();
    let query = args.get(1).cloned().unwrap_or_default();
    let file = args.get(2).cloned().unwrap_or_default();
    let me = current_self();
    let typing = all_typing_keys();

    if read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "") == "target" {
        if file.is_empty() {
            return 0;
        }
        if file == "__LOAD_MORE__" {
            let path = env_var("DIFFVIEW_RECENT_LIMIT_FILE", "");
            let cur_s = read_file(&path, "");
            let cur: u32 = if cur_s.is_empty() { 10 } else { cur_s.parse().unwrap_or(10) };
            write_file(&path, &(cur + 10).to_string());
            println!("reload-sync({} list)+refresh-preview+last", me);
            return 0;
        }
        write_file(&env_var("DIFFVIEW_TARGET_FILE", ""), &file);
        write_file(&env_var("DIFFVIEW_MODE_FILE", ""), "files");
        let saved = read_file(&env_var("DIFFVIEW_FILTER_FILE", ""), "");
        println!(
            "reload-sync({0} list)+disable-search+rebind({1})\
+change-prompt(diffview> )+change-query({2})+refresh-preview+first\
+change-header({3})+change-ghost({4})",
            me, typing, saved, header(), VIEW_GHOST
        );
        return 0;
    }

    if prompt.starts_with("send") {
        let saved = read_file(&env_var("DIFFVIEW_FILTER_FILE", ""), "");
        if !query.is_empty() {
            write_file(&env_var("DIFFVIEW_QUERY_FILE", ""), &query);
            println!(
                "execute-silent({0} send)+disable-search+rebind({1})\
+change-prompt(diffview> )+change-query({2})+change-ghost({3})",
                me, typing, saved, VIEW_GHOST
            );
        } else {
            println!(
                "disable-search+rebind({0})+change-prompt(diffview> )\
+change-query({1})+change-ghost({2})",
                typing, saved, VIEW_GHOST
            );
        }
        return 0;
    }

    write_file(&env_var("DIFFVIEW_FILTER_FILE", ""), &query);
    write_file(&env_var("DIFFVIEW_SEND_FILE", ""), &file);
    println!(
        "disable-search+unbind({})+change-prompt(send> )+clear-query+change-ghost()",
        typing
    );
    0
}

fn cmd_escape_action(args: &[String]) -> i32 {
    let prompt = args.first().cloned().unwrap_or_default();
    let me = current_self();
    let typing = all_typing_keys();

    if prompt.starts_with("filter") {
        if read_file(&env_var("DIFFVIEW_MODE_FILE", ""), "") == "target" {
            println!(
                "disable-search+rebind({})+change-prompt(target> )+change-ghost({})",
                typing, VIEW_GHOST
            );
        } else {
            println!(
                "disable-search+rebind({})+change-prompt(diffview> )+change-ghost({})",
                typing, VIEW_GHOST
            );
        }
        return 0;
    }
    if prompt.starts_with("target") {
        write_file(&env_var("DIFFVIEW_MODE_FILE", ""), "files");
        let saved = read_file(&env_var("DIFFVIEW_FILTER_FILE", ""), "");
        println!(
            "reload-sync({0} list)+disable-search+rebind({1})\
+change-prompt(diffview> )+change-query({2})+refresh-preview+first\
+change-header({3})+change-ghost({4})",
            me, typing, saved, header(), VIEW_GHOST
        );
        return 0;
    }
    if prompt.starts_with("send") {
        let saved = read_file(&env_var("DIFFVIEW_FILTER_FILE", ""), "");
        println!(
            "disable-search+rebind({0})+change-prompt(diffview> )\
+change-query({1})+change-ghost({2})",
            typing, saved, VIEW_GHOST
        );
    }
    0
}

fn cmd_preview(args: &[String]) -> i32 {
    let file = args.first().cloned().unwrap_or_default();
    Command::new("bash")
        .args(["-c", PREVIEW_CMD, "--", &file])
        .status()
        .map(|s| s.code().unwrap_or(0))
        .unwrap_or(127)
}

fn dispatch(a: &Args) -> Option<i32> {
    if a.subcmd.is_empty() {
        return None;
    }
    let rc = match a.subcmd.as_str() {
        "list" => cmd_list(),
        "header" => cmd_header(),
        "toggle" => cmd_toggle(&a.subcmd_args),
        "toggle-ws" => cmd_toggle_ws(),
        "toggle-sbs" => cmd_toggle_sbs(),
        "toggle-exclude" => cmd_toggle_exclude(),
        "toggle-target" => cmd_toggle_target(&a.subcmd_args),
        "enter-filter" => cmd_enter_filter(),
        "resize" => cmd_resize(&a.subcmd_args),
        "send" => cmd_send(),
        "context" => cmd_context(&a.subcmd_args),
        "open" => cmd_open(&a.subcmd_args), // never returns
        "enter-action" => cmd_enter_action(&a.subcmd_args),
        "escape-action" => cmd_escape_action(&a.subcmd_args),
        "preview" => cmd_preview(&a.subcmd_args),
        _ => 0,
    };
    Some(rc)
}

// --- state files ---

struct StateFiles {
    paths: Vec<PathBuf>,
}

impl Drop for StateFiles {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = fs::remove_file(p);
        }
    }
}

fn make_temp_file(content: &str) -> std::io::Result<PathBuf> {
    let tmpdir = env::temp_dir().join("diffview");
    fs::create_dir_all(&tmpdir)?;
    let pid = std::process::id();
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = tmpdir.join(format!("{}-{}", pid, n));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    f.write_all(content.as_bytes())?;
    Ok(path)
}

fn setup_state_files(a: &Args) -> StateFiles {
    let mode = if a.target_mode { "target" } else { "files" };
    let sbs = if a.sidebyside { "true" } else { "false" };
    let entries: &[(&str, String)] = &[
        ("DIFFVIEW_SIZE_FILE", "80".to_string()),
        ("DIFFVIEW_SEND_FILE", String::new()),
        ("DIFFVIEW_QUERY_FILE", String::new()),
        ("DIFFVIEW_FILTER_FILE", String::new()),
        ("DIFFVIEW_CONTEXT_FILE", DEFAULT_CONTEXT.to_string()),
        ("DIFFVIEW_WS_FILE", env_var("DIFFVIEW_WS", "")),
        ("DIFFVIEW_EXCLUDE_FILE", env_var("DIFFVIEW_EXCLUDE", "false")),
        ("DIFFVIEW_TARGET_FILE", env_var("DIFFVIEW_TARGET", "")),
        ("DIFFVIEW_SBS_FILE", sbs.to_string()),
        ("DIFFVIEW_MODE_FILE", mode.to_string()),
        ("DIFFVIEW_RECENT_LIMIT_FILE", "10".to_string()),
    ];
    let mut paths: Vec<PathBuf> = Vec::new();
    for (var, content) in entries {
        match make_temp_file(content) {
            Ok(p) => {
                unsafe { env::set_var(var, &p) };
                paths.push(p);
            }
            Err(e) => {
                eprintln!("diffview: failed to create state file {}: {}", var, e);
                std::process::exit(1);
            }
        }
    }
    StateFiles { paths }
}

// --- run fzf ---

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-./".contains(c)) {
        return s.to_string();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

fn run_fzf(a: &Args) -> i32 {
    let me = shell_quote(&current_self());

    let (output, initial_prompt) = if a.target_mode {
        (generate_target_list(), "target> ")
    } else {
        (generate_list(), "diffview> ")
    };
    let header_str = header();

    let binds: Vec<String> = vec![
        "ctrl-d:preview-half-page-down,ctrl-u:preview-half-page-up".to_string(),
        "page-down:preview-page-down,page-up:preview-page-up".to_string(),
        "home:preview-half-page-up,end:preview-half-page-down".to_string(),
        "left:up,right:down".to_string(),
        "up:preview-up,down:preview-down".to_string(),
        "shift-left:backward-char,shift-right:forward-char".to_string(),
        format!("ctrl-right:transform({} resize -3 {{fzf:prompt}})", me),
        format!("ctrl-left:transform({} resize +3 {{fzf:prompt}})", me),
        format!("focus:reload-sync({} list)", me),
        format!(
            "ctrl-up:execute-silent({0} context +3)+reload-sync({0} list)+refresh-preview",
            me
        ),
        format!(
            "ctrl-down:execute-silent({0} context -3)+reload-sync({0} list)+refresh-preview",
            me
        ),
        "start:disable-search".to_string(),
        format!("{}:ignore", IGNORE_KEYS),
        format!(
            "ctrl-e:execute({0} open {{2}})+reload-sync({0} list)+refresh-preview",
            me
        ),
        format!(
            "o:execute({0} open {{2}})+reload-sync({0} list)+refresh-preview",
            me
        ),
        format!("ctrl-w:transform({} toggle-ws)", me),
        format!("w:transform({} toggle-ws)", me),
        format!("ctrl-x:transform({} toggle-exclude)", me),
        format!("x:transform({} toggle-exclude)", me),
        format!("ctrl-g:transform({} toggle-target {{fzf:prompt}} {{q}})", me),
        format!("g:transform({} toggle-target {{fzf:prompt}} {{q}})", me),
        format!("ctrl-s:transform({} toggle-sbs)", me),
        format!("s:transform({} toggle-sbs)", me),
        "q:abort".to_string(),
        format!("/:transform({} enter-filter {{fzf:prompt}})", me),
        format!("enter:transform({} enter-action {{fzf:prompt}} {{q}} {{2}})", me),
        format!("esc:transform({} escape-action {{fzf:prompt}})", me),
        format!(
            "tab:execute-silent({0} toggle {{2}})+reload-sync({0} list)+transform([[ {{1}} == ✓* ]] || echo down)",
            me
        ),
    ];
    let truncated = output.lines().next().is_some_and(|l| l.contains("__TOO_MANY__"));
    let mut cmd = Command::new("fzf");
    cmd.arg("--ansi")
        .arg("--no-sort")
        .arg("--layout=reverse")
        .arg("--with-nth=1")
        .arg("--prompt").arg(initial_prompt)
        .arg("--ghost").arg(VIEW_GHOST)
        .arg("--color=ghost:8")
        .arg("--header").arg(&header_str)
        .arg("--delimiter").arg("\\x1f")
        .arg("--preview").arg(format!("bash -c '{}' -- {{2}}", PREVIEW_CMD))
        .arg("--preview-window=right:80%:wrap");
    if truncated {
        cmd.arg("--header-lines=1");
    }
    for b in &binds {
        cmd.arg("--bind").arg(b);
    }
    cmd.stdin(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("diffview: failed to spawn fzf: {}", e);
            return 127;
        }
    };
    if let Some(mut sin) = child.stdin.take() {
        let _ = sin.write_all(output.as_bytes());
    }
    match child.wait() {
        Ok(s) => s.code().unwrap_or(0),
        Err(_) => 1,
    }
}

const PREVIEW_CMD: &str = r#"
file="$1"
[[ -z "$file" ]] && exit 0
ws=$(cat "$DIFFVIEW_WS_FILE" 2>/dev/null)
ctx=$(cat "$DIFFVIEW_CONTEXT_FILE" 2>/dev/null || echo 4)
sbs_flag=""
[[ "$(cat "$DIFFVIEW_SBS_FILE" 2>/dev/null)" == "true" ]] && sbs_flag="--side-by-side"
mode=$(cat "$DIFFVIEW_MODE_FILE" 2>/dev/null)
if [[ "$mode" == "target" ]]; then
    [[ "$file" == "__LOAD_MORE__" ]] && exit 0
    cd "$DIFFVIEW_TOPLEVEL"
    {
        git diff -U$ctx $ws "$file"
        if [[ "$file" != *..* ]]; then
            while IFS= read -r u; do
                [[ -z "$u" || ! -f "$u" ]] && continue
                git diff -U$ctx $ws --no-index -- /dev/null "$u" 2>/dev/null
            done < <(git ls-files --others --exclude-standard)
        fi
    } | DELTA_FEATURES=inline delta $sbs_flag --wrap-max-lines unlimited --width="$FZF_PREVIEW_COLUMNS" --paging=never
    exit 0
fi
target=$(cat "$DIFFVIEW_TARGET_FILE" 2>/dev/null || echo "$DIFFVIEW_TARGET")
cd "$DIFFVIEW_TOPLEVEL"
if [[ "$target" == *..* ]]; then
    git diff -U$ctx $ws "$target" -- "$file" \
        | DELTA_FEATURES=inline delta $sbs_flag --wrap-max-lines unlimited --width="$FZF_PREVIEW_COLUMNS" --paging=never
elif [[ -f "$file" ]] && ! git ls-files --error-unmatch -- "$file" >/dev/null 2>&1; then
    git diff -U$ctx $ws --no-index -- /dev/null "$file" 2>/dev/null \
        | DELTA_FEATURES=inline delta $sbs_flag --wrap-max-lines unlimited --width="$FZF_PREVIEW_COLUMNS" --paging=never
elif git cat-file -e "$target:$file" 2>/dev/null; then
    git diff -U$ctx $ws "$target" -- "$file" \
        | DELTA_FEATURES=inline delta $sbs_flag --wrap-max-lines unlimited --width="$FZF_PREVIEW_COLUMNS" --paging=never
else
    old_path=$(git diff --name-status $ws "$target" 2>/dev/null | grep -P "^R\d+\t.+\t\Q${file}\E$" | head -1 | cut -f2)
    if [[ -n "$old_path" ]]; then
        git diff -U$ctx $ws "$target" -- "$old_path" "$file" \
            | DELTA_FEATURES=inline delta $sbs_flag --wrap-max-lines unlimited --width="$FZF_PREVIEW_COLUMNS" --paging=never
    elif [[ -f "$file" ]]; then
        git diff -U$ctx $ws --no-index -- /dev/null "$file" 2>/dev/null \
            | DELTA_FEATURES=inline delta $sbs_flag --wrap-max-lines unlimited --width="$FZF_PREVIEW_COLUMNS" --paging=never
    fi
fi
"#;

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    if argv.iter().any(|s| s == "--help") {
        println!("fzf-driven git diff viewer.");
        println!("Usage: diffview [wsxmpt|~N ...]  (w=ws, s=sbs, x=excl-gen, m=merge-base, p=upstream, t=target picker, ~N=HEAD~N)");
        std::process::exit(0);
    }
    let mut a = parse_args(&argv);
    init_state(&mut a);
    if let Some(rc) = dispatch(&a) {
        std::process::exit(rc);
    }
    let _state = setup_state_files(&a);
    let rc = run_fzf(&a);
    std::process::exit(rc);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn format_stats_basic() {
        assert_eq!(format_stats(" 3 files changed, 5 insertions(+), 2 deletions(-)"), "+5/-2");
    }

    #[test]
    fn format_stats_insertions_only() {
        assert_eq!(format_stats(" 1 file changed, 7 insertions(+)"), "+7/-0");
    }

    #[test]
    fn format_stats_deletions_only() {
        assert_eq!(format_stats(" 1 file changed, 4 deletions(-)"), "+0/-4");
    }

    #[test]
    fn format_stats_empty() {
        assert_eq!(format_stats(""), "");
    }

    #[test]
    fn format_stats_no_changes() {
        assert_eq!(format_stats(" 0 files changed"), "");
    }

    #[test]
    fn shorten_path_short() {
        assert_eq!(shorten_path("foo.py"), "foo.py");
    }

    #[test]
    fn shorten_path_keeps_last_segment() {
        assert_eq!(shorten_path("foo/bar/baz.py"), "fo/ba/baz.py");
    }

    #[test]
    fn parse_args_flags() {
        let a = parse_args(&s(&["w", "x", "s", "m", "p"]));
        assert_eq!(a.ws, "-w");
        assert!(a.exclude);
        assert!(a.sidebyside);
        assert!(a.merge_base);
        assert!(a.upstream);
        assert_eq!(a.subcmd, "");
    }

    #[test]
    fn parse_args_commit_back() {
        let a = parse_args(&s(&["~3"]));
        assert_eq!(a.commit_back, "3");
    }

    #[test]
    fn parse_args_subcommand_swallows_rest() {
        let a = parse_args(&s(&["w", "list", "extra", "args"]));
        assert_eq!(a.ws, "-w");
        assert_eq!(a.subcmd, "list");
        assert_eq!(a.subcmd_args, vec!["extra", "args"]);
    }

    #[test]
    fn parse_args_target_mode() {
        assert!(parse_args(&s(&["t"])).target_mode);
    }

    #[test]
    fn parse_args_upstream_flag() {
        assert!(parse_args(&s(&["p"])).upstream);
    }

    #[test]
    fn parse_rename_path_plain() {
        assert_eq!(
            parse_rename_path("old.py => new.py"),
            Some(("old.py".to_string(), "new.py".to_string()))
        );
    }

    #[test]
    fn parse_rename_path_brace_with_prefix_and_suffix() {
        assert_eq!(
            parse_rename_path("a/{old => new}/c"),
            Some(("a/old/c".to_string(), "a/new/c".to_string()))
        );
    }

    #[test]
    fn parse_rename_path_brace_empty_new_collapses_slash() {
        assert_eq!(
            parse_rename_path("dir/{sub => }/file.hpp"),
            Some(("dir/sub/file.hpp".to_string(), "dir/file.hpp".to_string()))
        );
    }

    #[test]
    fn parse_rename_path_brace_empty_old_collapses_slash() {
        assert_eq!(
            parse_rename_path("dir/{ => sub}/file.hpp"),
            Some(("dir/file.hpp".to_string(), "dir/sub/file.hpp".to_string()))
        );
    }

    #[test]
    fn parse_rename_path_brace_only_prefix() {
        assert_eq!(
            parse_rename_path("dir/{old.hpp => new.hpp}"),
            Some(("dir/old.hpp".to_string(), "dir/new.hpp".to_string()))
        );
    }

    #[test]
    fn parse_rename_path_no_arrow() {
        assert_eq!(parse_rename_path("just/a/path.py"), None);
    }
}
