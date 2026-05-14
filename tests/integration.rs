// Black-box integration tests for cc-diffview.
//
// Pure-function tests live in src/main.rs (`#[cfg(test)] mod tests`); this file
// only tests observable subprocess behavior.

use std::fs;

mod common;
use common::{Repo, State, Stubs, parse_list_rows, run, run_with, strip_ansi};

// --- helpers ---

fn header_state(state: &mut State, repo: &Repo, target: &str, mode: &str, ws: &str, sbs: &str, exclude: &str) {
    state.file("DIFFVIEW_TARGET_FILE", target);
    state.file("DIFFVIEW_MODE_FILE", mode);
    state.file("DIFFVIEW_WS_FILE", ws);
    state.file("DIFFVIEW_SBS_FILE", sbs);
    state.file("DIFFVIEW_EXCLUDE_FILE", exclude);
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    state.setenv("DIFFVIEW_MERGE_BASE_TARGET", &head);
    state.setenv("DIFFVIEW_TARGET", target);
}

fn header_state_default(state: &mut State, repo: &Repo) {
    header_state(state, repo, "HEAD", "files", "", "false", "false");
}

fn list_env(state: &mut State, repo: &Repo, target: &str, exclude: &str, ws: &str) {
    state.file("DIFFVIEW_TARGET_FILE", target);
    state.file("DIFFVIEW_EXCLUDE_FILE", exclude);
    state.file("DIFFVIEW_WS_FILE", ws);
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_TARGET", target);
}

// --- cmd_context ---

#[test]
fn cmd_context_clamps_high() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let p = state.file("DIFFVIEW_CONTEXT_FILE", "5");
    let r = run(&repo, &state, &stubs, &["context", "+1000"]);
    assert_eq!(r.code, 0);
    assert_eq!(fs::read_to_string(&p).unwrap(), "999");
}

#[test]
fn cmd_context_clamps_low() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let p = state.file("DIFFVIEW_CONTEXT_FILE", "5");
    run(&repo, &state, &stubs, &["context", "-100"]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "0");
}

// --- cmd_resize ---

#[test]
fn cmd_resize_writes_size() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let p = state.file("DIFFVIEW_SIZE_FILE", "80");
    let r = run(&repo, &state, &stubs, &["resize", "+3", "diffview> "]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "83");
    assert!(r.stdout.contains("change-preview-window(right,83%,wrap)"));
}

#[test]
fn cmd_resize_in_filter_prompt_navigates() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_SIZE_FILE", "80");
    let r = run(&repo, &state, &stubs, &["resize", "+3", "filter> "]);
    assert_eq!(r.stdout.trim(), "backward-word");
}

// --- cmd_send ---

#[test]
fn cmd_send_no_query_returns_zero() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_SEND_FILE", "");
    state.file("DIFFVIEW_QUERY_FILE", "");
    stubs.record("tmux", "");
    let r = run(&repo, &state, &stubs, &["send"]);
    assert_eq!(r.code, 0);
}

#[test]
fn cmd_send_with_query_calls_tmux() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_SEND_FILE", "src/foo.py");
    state.file("DIFFVIEW_QUERY_FILE", "explain this");
    stubs.record(
        "tmux",
        r#"case "$1" in
    show-environment) echo "DIFFVIEW_CALLER=%42" ;;
esac"#,
    );
    let r = run(&repo, &state, &stubs, &["send"]);
    assert_eq!(r.code, 0);
    let calls = stubs.calls("tmux");
    assert_eq!(
        calls[1],
        vec!["send-keys", "-t", "%42", "-l", "In src/foo.py: explain this"]
    );
    assert_eq!(calls[2], vec!["send-keys", "-t", "%42", "Enter"]);
}

#[test]
fn cmd_send_strips_tmux_from_env() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_SEND_FILE", "");
    state.file("DIFFVIEW_QUERY_FILE", "hi");
    stubs.add(
        "tmux",
        r#"if [[ -n "$TMUX" ]]; then
    echo "TMUX leaked: $TMUX" >&2
    exit 99
fi
case "$1" in
    show-environment) echo "DIFFVIEW_CALLER=%1" ;;
esac
exit 0"#,
    );
    let r = run_with(
        &repo, &state, &stubs, &["send"],
        &[("TMUX", "/tmp/popup,123,0")],
    );
    assert_eq!(r.code, 0);
    assert!(!r.stderr.contains("TMUX leaked"));
}

// --- cmd_open ---

#[test]
fn cmd_open_extracts_first_hunk_line() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let big: String = (1..60).map(|i| format!("line {}\n", i)).collect();
    repo.write("foo.py", &big);
    repo.commit_all("init foo");
    let mut lines: Vec<&str> = big.lines().collect();
    lines[41] = "MODIFIED";
    let modified: String = lines.iter().map(|l| format!("{}\n", l)).collect();
    repo.write("foo.py", &modified);

    state.file("DIFFVIEW_MODE_FILE", "files");
    state.file("DIFFVIEW_TARGET_FILE", "HEAD");
    state.file("DIFFVIEW_WS_FILE", "");
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    stubs.record("nvim", "");

    let r = run(&repo, &state, &stubs, &["open", "foo.py"]);
    assert_eq!(r.code, 0);
    let calls = stubs.calls("nvim");
    assert_eq!(calls.len(), 1);
    assert!(calls[0][0].starts_with('+'));
    let line: i32 = calls[0][0][1..].parse().unwrap();
    assert!(line >= 30 && line <= 45, "expected hunk line ~42, got {}", line);
    assert_eq!(calls[0][1], "foo.py");
}

#[test]
fn cmd_open_defaults_to_line_one_without_hunk() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("foo.py", "seed\n");
    repo.commit_all("init foo");
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.file("DIFFVIEW_TARGET_FILE", "HEAD");
    state.file("DIFFVIEW_WS_FILE", "");
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    stubs.record("nvim", "");

    let r = run(&repo, &state, &stubs, &["open", "foo.py"]);
    assert_eq!(r.code, 0);
    let calls = stubs.calls("nvim");
    assert_eq!(calls[0][0], "+1");
}

#[test]
fn cmd_open_noop_in_target_mode() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "target");
    stubs.record("nvim", "");
    let r = run(&repo, &state, &stubs, &["open", "foo.py"]);
    assert_eq!(r.code, 0);
    assert!(stubs.calls("nvim").is_empty());
}

// --- cmd_toggle (viewed marking) ---

#[test]
fn cmd_toggle_marks_file_viewed() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("foo.py", "hello\n");
    let viewed = repo.path.join(".git/diff-viewed");
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_VIEWED", viewed.to_string_lossy().as_ref());
    let h = repo.hash_object("foo.py");

    run(&repo, &state, &stubs, &["toggle", "foo.py"]);
    assert_eq!(fs::read_to_string(&viewed).unwrap(), format!("{} foo.py\n", h));
}

#[test]
fn cmd_toggle_unmarks_when_already_viewed_at_same_hash() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("foo.py", "hello\n");
    let h = repo.hash_object("foo.py");
    let viewed = repo.path.join(".git/diff-viewed");
    fs::write(&viewed, format!("{} foo.py\nother bar.py\n", h)).unwrap();
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_VIEWED", viewed.to_string_lossy().as_ref());

    run(&repo, &state, &stubs, &["toggle", "foo.py"]);
    let text = fs::read_to_string(&viewed).unwrap();
    assert!(!text.contains("foo.py"), "got: {:?}", text);
    assert!(text.contains("bar.py"));
}

#[test]
fn cmd_toggle_noop_in_target_mode() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "target");
    let r = run(&repo, &state, &stubs, &["toggle", "foo.py"]);
    assert_eq!(r.code, 0);
}

// --- cmd_toggle_ws / sbs / exclude ---

#[test]
fn cmd_toggle_ws_flips() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state_default(&mut state, &repo);
    let p = state.file("DIFFVIEW_WS_FILE", "");

    let r = run(&repo, &state, &stubs, &["toggle-ws"]);
    assert_eq!(r.code, 0);
    assert_eq!(fs::read_to_string(&p).unwrap(), "-w");
    assert!(r.stdout.contains("reload-sync"));
    assert!(r.stdout.contains("change-header"));

    run(&repo, &state, &stubs, &["toggle-ws"]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "");
}

#[test]
fn cmd_toggle_sbs_flips() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state_default(&mut state, &repo);
    let p = state.file("DIFFVIEW_SBS_FILE", "false");

    run(&repo, &state, &stubs, &["toggle-sbs"]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "true");
    run(&repo, &state, &stubs, &["toggle-sbs"]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "false");
}

#[test]
fn cmd_toggle_exclude_flips() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state_default(&mut state, &repo);
    let p = state.file("DIFFVIEW_EXCLUDE_FILE", "false");

    run(&repo, &state, &stubs, &["toggle-exclude"]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "true");
    run(&repo, &state, &stubs, &["toggle-exclude"]);
    assert_eq!(fs::read_to_string(&p).unwrap(), "false");
}

// --- cmd_toggle_target ---

#[test]
fn cmd_toggle_target_switches_to_target_mode() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state_default(&mut state, &repo);
    let mode = state.file("DIFFVIEW_MODE_FILE", "files");
    let filt = state.file("DIFFVIEW_FILTER_FILE", "");

    let r = run(&repo, &state, &stubs, &["toggle-target", "diffview> ", "myquery"]);
    assert_eq!(fs::read_to_string(&mode).unwrap(), "target");
    assert_eq!(fs::read_to_string(&filt).unwrap(), "myquery");
    assert!(r.stdout.contains("change-prompt(target> )"));
    assert!(r.stdout.contains("disable-search"));
    assert!(r.stdout.contains("clear-query"));
}

#[test]
fn cmd_toggle_target_noop_in_target_mode() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "target");
    let r = run(&repo, &state, &stubs, &["toggle-target", "target> ", "q"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "");
}

#[test]
fn cmd_toggle_target_noop_in_send_prompt() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "files");
    let r = run(&repo, &state, &stubs, &["toggle-target", "send> ", "q"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "");
}

// --- cmd_enter_action ---

#[test]
fn cmd_enter_action_target_mode_picks_target() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state(&mut state, &repo, "HEAD", "target", "", "false", "false");
    let target_file = state.file("DIFFVIEW_TARGET_FILE", "");
    state.file("DIFFVIEW_MODE_FILE", "target");
    state.file("DIFFVIEW_FILTER_FILE", "previous-filter");

    let r = run(
        &repo, &state, &stubs,
        &["enter-action", "target> ", "q", "abc123^..abc123"],
    );
    assert_eq!(fs::read_to_string(&target_file).unwrap(), "abc123^..abc123");
    assert!(r.stdout.contains("change-prompt(diffview> )"));
    assert!(r.stdout.contains("change-query(previous-filter)"));
}

#[test]
fn cmd_enter_action_target_mode_load_more() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "target");
    let limit = state.file("DIFFVIEW_RECENT_LIMIT_FILE", "10");
    let r = run(
        &repo, &state, &stubs,
        &["enter-action", "target> ", "", "__LOAD_MORE__"],
    );
    assert_eq!(fs::read_to_string(&limit).unwrap(), "20");
    assert!(r.stdout.contains("reload-sync"));
}

#[test]
fn cmd_enter_action_send_prompt_with_query_triggers_send() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "files");
    let query_file = state.file("DIFFVIEW_QUERY_FILE", "");
    state.file("DIFFVIEW_FILTER_FILE", "saved");
    state.file("DIFFVIEW_SEND_FILE", "");
    stubs.record("tmux", "");

    let r = run(
        &repo, &state, &stubs,
        &["enter-action", "send> ", "the question", "src/foo.py"],
    );
    assert_eq!(fs::read_to_string(&query_file).unwrap(), "the question");
    assert!(r.stdout.contains("execute-silent"));
    assert!(r.stdout.contains("send"));
    assert!(r.stdout.contains("change-query(saved)"));
}

#[test]
fn cmd_enter_action_send_prompt_without_query_just_restores() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.file("DIFFVIEW_QUERY_FILE", "");
    state.file("DIFFVIEW_FILTER_FILE", "saved");

    let r = run(
        &repo, &state, &stubs,
        &["enter-action", "send> ", "", "src/foo.py"],
    );
    assert!(!r.stdout.contains("execute-silent"));
    assert!(r.stdout.contains("change-prompt(diffview> )"));
}

#[test]
fn cmd_enter_action_default_enters_send_prompt() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "files");
    let filt = state.file("DIFFVIEW_FILTER_FILE", "");
    let send = state.file("DIFFVIEW_SEND_FILE", "");

    let r = run(
        &repo, &state, &stubs,
        &["enter-action", "diffview> ", "user-typed", "src/foo.py"],
    );
    assert_eq!(fs::read_to_string(&filt).unwrap(), "user-typed");
    assert_eq!(fs::read_to_string(&send).unwrap(), "src/foo.py");
    assert!(r.stdout.contains("change-prompt(send> )"));
    assert!(r.stdout.contains("clear-query"));
}

// --- cmd_escape_action ---

#[test]
fn cmd_escape_action_from_filter_returns_to_diffview() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "files");
    let r = run(&repo, &state, &stubs, &["escape-action", "filter> "]);
    assert!(r.stdout.contains("change-prompt(diffview> )"));
}

#[test]
fn cmd_escape_action_from_filter_in_target_mode() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "target");
    let r = run(&repo, &state, &stubs, &["escape-action", "filter> "]);
    assert!(r.stdout.contains("change-prompt(target> )"));
}

#[test]
fn cmd_escape_action_from_target_switches_to_files() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state(&mut state, &repo, "HEAD", "target", "", "false", "false");
    let mode = state.file("DIFFVIEW_MODE_FILE", "target");
    state.file("DIFFVIEW_FILTER_FILE", "saved-filter");

    let r = run(&repo, &state, &stubs, &["escape-action", "target> "]);
    assert_eq!(fs::read_to_string(&mode).unwrap(), "files");
    assert!(r.stdout.contains("change-query(saved-filter)"));
    assert!(r.stdout.contains("change-prompt(diffview> )"));
}

#[test]
fn cmd_escape_action_from_send_restores() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.file("DIFFVIEW_FILTER_FILE", "back-to-this");
    let r = run(&repo, &state, &stubs, &["escape-action", "send> "]);
    assert!(r.stdout.contains("change-prompt(diffview> )"));
    assert!(r.stdout.contains("change-query(back-to-this)"));
}

// --- cmd_enter_filter ---

#[test]
fn cmd_enter_filter_emits_search_actions() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let state = State::new();
    let r = run(&repo, &state, &stubs, &["enter-filter"]);
    assert!(r.stdout.contains("enable-search"));
    assert!(r.stdout.contains("change-prompt(filter> )"));
}

// --- header ---

#[test]
fn header_uncommitted_no_changes() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state_default(&mut state, &repo);
    let r = run(&repo, &state, &stubs, &["header"]);
    assert_eq!(r.stdout.trim(), "u");
}

#[test]
fn header_uncommitted_with_stats() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed\nadded\n");
    header_state_default(&mut state, &repo);
    let r = run(&repo, &state, &stubs, &["header"]);
    assert_eq!(r.stdout.trim(), "u +1/-0");
}

#[test]
fn header_merge_base() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "changed\n");
    repo.commit_all("second");
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    repo.write("init.txt", "changed\nextra\n");
    state.file("DIFFVIEW_TARGET_FILE", &head);
    state.file("DIFFVIEW_MODE_FILE", "files");
    state.file("DIFFVIEW_WS_FILE", "");
    state.file("DIFFVIEW_SBS_FILE", "false");
    state.file("DIFFVIEW_EXCLUDE_FILE", "false");
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_MERGE_BASE_TARGET", &head);
    state.setenv("DIFFVIEW_TARGET", &head);

    let r = run(&repo, &state, &stubs, &["header"]);
    let out = r.stdout.trim();
    assert!(out.starts_with("m "), "got: {}", out);
    assert!(out.contains("+1/-0"));
}

#[test]
fn header_commit_back_range() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed\nx\n");
    repo.commit_all("second");
    header_state(&mut state, &repo, "HEAD~1^..HEAD~1", "files", "", "false", "false");
    let r = run(&repo, &state, &stubs, &["header"]);
    assert!(r.stdout.trim().starts_with('~'));
}

#[test]
fn header_flags_only() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state(&mut state, &repo, "HEAD", "files", "-w", "true", "true");
    let r = run(&repo, &state, &stubs, &["header"]);
    assert_eq!(r.stdout.trim(), "s w x · u");
}

#[test]
fn header_flag_order() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    header_state(&mut state, &repo, "HEAD", "files", "-w", "false", "true");
    let r = run(&repo, &state, &stubs, &["header"]);
    assert!(r.stdout.trim().starts_with("w x"));
}

#[test]
fn header_target_mode_omits_target_suffix() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed\nx\n");
    header_state(&mut state, &repo, "HEAD", "target", "-w", "false", "false");
    let r = run(&repo, &state, &stubs, &["header"]);
    let out = r.stdout.trim().to_string();
    let rhs = out.split('·').last().unwrap_or("").trim();
    let rhs_words: Vec<&str> = rhs.split_whitespace().collect();
    assert!(!rhs_words.contains(&"u"));
    assert!(out.contains("+1/-0"));
}

// --- generate_list ---

#[test]
fn generate_list_basic() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("src/a.py", &"a\n".repeat(5));
    repo.write("src/b.py", "b\n");
    repo.commit_all("src");
    repo.write("src/a.py", &("aa\n".repeat(5) + "extra\n"));
    repo.write("src/b.py", "b\nadd1\nadd2\n");
    list_env(&mut state, &repo, "HEAD", "false", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let targets: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
    assert!(targets.contains(&"src/a.py"));
    assert!(targets.contains(&"src/b.py"));
}

#[test]
fn generate_list_ws_mode_includes_whitespace_only_change() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("a.txt", "foo bar\n");
    repo.write("b.txt", "x\n");
    repo.commit_all("seed");
    repo.write("a.txt", "foo  bar\n");
    repo.write("b.txt", "y\n");
    list_env(&mut state, &repo, "HEAD", "false", "-w");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let targets: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
    assert!(targets.contains(&"a.txt"), "whitespace-only change should still appear: {:?}", targets);
    assert!(targets.contains(&"b.txt"));
}

#[test]
fn generate_list_rename_suffix() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("old.py", &"hello\n".repeat(20));
    repo.commit_all("old");
    fs::rename(repo.path.join("old.py"), repo.path.join("new.py")).unwrap();
    list_env(&mut state, &repo, "HEAD", "false", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    assert!(rows.iter().any(|(_, t)| t == "new.py"));
}

#[test]
fn generate_list_viewed_marker_shown_when_hash_matches() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("foo.py", "x\n");
    repo.commit_all("foo");
    repo.write("foo.py", "y\n");
    let h = repo.hash_object("foo.py");
    let viewed = repo.path.join(".git/diff-viewed");
    fs::write(&viewed, format!("{} foo.py\n", h)).unwrap();
    list_env(&mut state, &repo, "HEAD", "false", "");
    state.setenv("DIFFVIEW_VIEWED", viewed.to_string_lossy().as_ref());

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    assert!(rows.iter().any(|(d, t)| d.starts_with("✓ ") && t == "foo.py"),
        "rows: {:?}", rows);
}

#[test]
fn generate_list_viewed_marker_dropped_when_hash_changed() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("foo.py", "x\n");
    repo.commit_all("foo");
    repo.write("foo.py", "y\n");
    let viewed = repo.path.join(".git/diff-viewed");
    fs::write(&viewed, "oldhash foo.py\n").unwrap();
    list_env(&mut state, &repo, "HEAD", "false", "");
    state.setenv("DIFFVIEW_VIEWED", viewed.to_string_lossy().as_ref());

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    assert!(rows.iter().any(|(d, t)| t == "foo.py" && !d.starts_with("✓")));
}

#[test]
fn generate_list_viewed_marker_for_deleted_file() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("foo.py", &"hi\n".repeat(5));
    repo.commit_all("foo");
    fs::remove_file(repo.path.join("foo.py")).unwrap();
    let viewed = repo.path.join(".git/diff-viewed");
    fs::write(&viewed, "DELETED foo.py\n").unwrap();
    list_env(&mut state, &repo, "HEAD", "false", "");
    state.setenv("DIFFVIEW_VIEWED", viewed.to_string_lossy().as_ref());

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    assert!(rows.iter().any(|(d, t)| t == "foo.py" && d.starts_with("✓")));
}

#[test]
fn generate_list_exclude_drops_generated() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("src/a.py", "a\n");
    repo.write("src/generated/b.py", "b\n");
    repo.commit_all("init src");
    repo.write("src/a.py", "aa\n");
    repo.write("src/generated/b.py", "bb\n");
    list_env(&mut state, &repo, "HEAD", "true", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let targets: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
    assert!(targets.contains(&"src/a.py"));
    assert!(!targets.contains(&"src/generated/b.py"));
}

#[test]
fn generate_list_untracked_file_gets_line_count() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("new.py", "a\nb\nc\n");
    list_env(&mut state, &repo, "HEAD", "false", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let m: Vec<&String> = rows.iter().filter(|(_, t)| t == "new.py").map(|(d, _)| d).collect();
    assert!(!m.is_empty(), "rows: {:?}", rows);
    assert!(m[0].contains("(+3/-0)"), "got: {:?}", m[0]);
}

#[test]
fn generate_list_caps_at_500_with_sentinel() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    for i in 0..501 {
        repo.write(&format!("u{:04}.txt", i), "x\n");
    }
    list_env(&mut state, &repo, "HEAD", "false", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    assert_eq!(r.code, 0);
    let rows = parse_list_rows(&r.stdout);
    assert_eq!(rows.len(), 501, "want 1 sentinel + 500 files");
    let (display, target) = &rows[0];
    assert_eq!(target, "__TOO_MANY__");
    let plain = strip_ansi(display);
    assert!(plain.contains("Showing 500 of 501"), "sentinel text: {:?}", plain);
}

#[test]
fn generate_list_no_sentinel_under_cap() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    for i in 0..10 {
        repo.write(&format!("u{:02}.txt", i), "x\n");
    }
    list_env(&mut state, &repo, "HEAD", "false", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    assert!(!rows.iter().any(|(_, t)| t == "__TOO_MANY__"));
}

#[test]
fn generate_list_range_target_skips_untracked() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("tracked.py", "hi\n");
    repo.commit_all("add tracked");
    repo.write("untracked.py", "nope\n");
    list_env(&mut state, &repo, "HEAD^..HEAD", "false", "");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let targets: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
    assert!(targets.contains(&"tracked.py"));
    assert!(!targets.contains(&"untracked.py"));
}

// --- generate_target_list ---

#[test]
fn generate_target_list_no_merge_base() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_WS_FILE", "");
    state.file("DIFFVIEW_RECENT_LIMIT_FILE", "10");
    state.file("DIFFVIEW_MODE_FILE", "target");
    state.file("DIFFVIEW_TARGET_FILE", "HEAD");
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    state.setenv("DIFFVIEW_MERGE_BASE_TARGET", &head);
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_TARGET", "HEAD");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let targets: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(targets[0], "HEAD");
    let short = repo.git(&["rev-parse", "--short", "HEAD"]).trim().to_string();
    assert!(
        targets.iter().any(|&t| t.ends_with(&format!("..{}", short))),
        "no per-commit row for HEAD: {:?}",
        targets,
    );
}

#[test]
fn generate_target_list_with_merge_base() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let init = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    repo.write("x.py", "hi\n");
    repo.commit_all("second");

    state.file("DIFFVIEW_WS_FILE", "");
    state.file("DIFFVIEW_RECENT_LIMIT_FILE", "10");
    state.file("DIFFVIEW_MODE_FILE", "target");
    state.file("DIFFVIEW_TARGET_FILE", "HEAD");
    state.setenv("DIFFVIEW_MERGE_BASE_TARGET", &init);
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_TARGET", "HEAD");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);
    let displays: Vec<&str> = rows.iter().map(|(d, _)| d.as_str()).collect();
    assert!(displays.iter().any(|d| d.contains("Uncommitted")));
    assert!(displays.iter().any(|d| d.contains("Since merge base")));
}

#[test]
fn generate_target_list_root_commit_target_diffs_cleanly() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    state.file("DIFFVIEW_WS_FILE", "");
    state.file("DIFFVIEW_RECENT_LIMIT_FILE", "10");
    state.file("DIFFVIEW_MODE_FILE", "target");
    state.file("DIFFVIEW_TARGET_FILE", "HEAD");
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    state.setenv("DIFFVIEW_MERGE_BASE_TARGET", &head);
    state.setenv("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref());
    state.setenv("DIFFVIEW_TARGET", "HEAD");

    let r = run(&repo, &state, &stubs, &["list"]);
    let rows = parse_list_rows(&r.stdout);

    let short = repo.git(&["rev-parse", "--short", "HEAD"]).trim().to_string();
    let row = rows.iter().find(|(_, t)| t.ends_with(&format!("..{}", short)) && t.as_str() != "HEAD");
    let target = row.map(|(_, t)| t.as_str()).unwrap_or_else(|| panic!("no per-commit row found: {:?}", rows));

    let out = std::process::Command::new("git")
        .args(["diff", "--numstat", target])
        .current_dir(&repo.path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git diff failed for root-commit target {:?}: stderr={}",
        target,
        String::from_utf8_lossy(&out.stderr),
    );
}

// --- preview ---

fn run_preview(
    repo: &Repo,
    state: &mut State,
    stubs: &Stubs,
    file: &str,
    target: &str,
    ws: &str,
    sbs: &str,
    mode: &str,
) -> (String, i32) {
    state.file("DIFFVIEW_WS_FILE", ws);
    state.file("DIFFVIEW_CONTEXT_FILE", "4");
    state.file("DIFFVIEW_SBS_FILE", sbs);
    state.file("DIFFVIEW_MODE_FILE", mode);
    state.file("DIFFVIEW_TARGET_FILE", target);
    let r = run_with(
        repo, state, stubs, &["preview", file],
        &[
            ("DIFFVIEW_TOPLEVEL", repo.path.to_string_lossy().as_ref()),
            ("DIFFVIEW_TARGET", target),
            ("FZF_PREVIEW_COLUMNS", "120"),
        ],
    );
    (strip_ansi(&r.stdout), r.code)
}

#[test]
fn preview_empty_file_arg_exits_quietly() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let (out, code) = run_preview(&repo, &mut state, &stubs, "", "HEAD", "", "false", "files");
    assert_eq!(code, 0);
    assert_eq!(out, "");
}

#[test]
fn preview_files_mode_modified_file() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed\nadded\n");
    let (out, code) = run_preview(&repo, &mut state, &stubs, "init.txt", "HEAD", "", "false", "files");
    assert_eq!(code, 0);
    assert!(out.contains("added"), "got: {:?}", out);
}

#[test]
fn preview_files_mode_untracked_file() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("new.txt", "brand new line\n");
    let (out, code) = run_preview(&repo, &mut state, &stubs, "new.txt", "HEAD", "", "false", "files");
    assert_eq!(code, 0);
    assert!(out.contains("brand new line"));
}

#[test]
fn preview_files_mode_range_target() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed\nsecond\n");
    repo.commit_all("second");
    let (out, code) = run_preview(&repo, &mut state, &stubs, "init.txt", "HEAD~1..HEAD", "", "false", "files");
    assert_eq!(code, 0);
    assert!(out.contains("second"));
}

#[test]
fn preview_files_mode_renamed_file() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    fs::rename(repo.path.join("init.txt"), repo.path.join("renamed.txt")).unwrap();
    repo.write("renamed.txt", "seed\nextra\n");
    repo.commit_all("rename");
    let (out, code) = run_preview(&repo, &mut state, &stubs, "renamed.txt", "HEAD~1", "", "false", "files");
    assert_eq!(code, 0);
    assert!(out.contains("extra"));
}

#[test]
fn preview_untracked_file_at_renamed_away_path() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.git(&["mv", "init.txt", "renamed.txt"]);
    repo.write("init.txt", "fresh content\n");
    let (out, code) = run_preview(&repo, &mut state, &stubs, "init.txt", "HEAD", "", "false", "files");
    assert_eq!(code, 0);
    assert!(out.contains("fresh content"), "expected new content, got: {out:?}");
    assert!(!out.contains("seed"), "should not show old content as deleted, got: {out:?}");
}

#[test]
fn preview_target_mode_for_commit_range() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed\ninline\n");
    repo.commit_all("second");
    let rev = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    let target = format!("{}^..{}", rev, rev);
    let (out, code) = run_preview(&repo, &mut state, &stubs, &target, &target, "", "false", "target");
    assert_eq!(code, 0);
    assert!(out.contains("inline"));
}

#[test]
fn preview_target_mode_load_more_sentinel_is_noop() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    let (out, code) = run_preview(&repo, &mut state, &stubs, "__LOAD_MORE__", "HEAD", "", "false", "target");
    assert_eq!(code, 0);
    assert_eq!(out, "");
}

#[test]
fn preview_respects_whitespace_flag() {
    let repo = Repo::new();
    let stubs = Stubs::new();
    let mut state = State::new();
    repo.write("init.txt", "seed   \n");
    let (out_no_ws, _) = run_preview(&repo, &mut state, &stubs, "init.txt", "HEAD", "", "false", "files");
    let mut state2 = State::new();
    let (out_w, _) = run_preview(&repo, &mut state2, &stubs, "init.txt", "HEAD", "-w", "false", "files");
    assert!(out_no_ws.contains("seed"));
    assert!(!out_w.contains("seed") || out_w.trim().is_empty());
}
