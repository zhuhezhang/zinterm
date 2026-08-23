use super::{
    history_summary, history_view_rows, test_app_command_capture, test_capture_should_suppress,
    test_capture_should_suppress_at,
};
use std::time::{Duration, Instant};

#[test]
fn lists_and_filters_commands_newest_first() {
    let history = vec![
        "git status".to_string(),
        "cargo check".to_string(),
        "git log".to_string(),
    ];

    let all: Vec<String> = history_view_rows(&history, "")
        .into_iter()
        .map(|row| row.command.into())
        .collect();
    assert_eq!(all, ["git log", "cargo check", "git status"]);

    let filtered: Vec<String> = history_view_rows(&history, "GIT")
        .into_iter()
        .map(|row| row.command.into())
        .collect();
    assert_eq!(filtered, ["git log", "git status"]);
}

#[test]
fn suppresses_terminal_capture_for_app_multiline_commands() {
    let mut state = test_app_command_capture("echo first\necho second");
    assert!(test_capture_should_suppress(&mut state, "echo first"));
    assert!(test_capture_should_suppress(&mut state, "echo second"));
    assert!(!test_capture_should_suppress(
        &mut test_app_command_capture("echo first\necho second"),
        "echo third"
    ));
}

#[test]
fn suppresses_terminal_capture_for_full_multiline_shell_report() {
    let mut state = test_app_command_capture("cat <<'EOF'\nline\nEOF");
    assert!(test_capture_should_suppress(
        &mut state,
        "cat <<'EOF'\nline\nEOF"
    ));
}

#[test]
fn expired_app_command_capture_is_not_suppressed() {
    assert!(!test_capture_should_suppress_at(
        "echo hi",
        Instant::now() - Duration::from_secs(31),
        "echo hi"
    ));
}

#[test]
fn summary_collapses_multiline_commands() {
    assert_eq!(history_summary("git status"), "git status");
    assert_eq!(
        history_summary("echo first\nsecond line"),
        "echo first…"
    );
    assert_eq!(
        history_summary("echo first\r\nsecond line"),
        "echo first…"
    );
    assert_eq!(history_summary("\n\nonly later"), "only later…");
    assert_eq!(history_summary("\n"), "…");
}
