use std::fs;
use std::process::Command;

#[test]
fn invalid_arguments_fail_before_loading_history() {
    let cases: &[(&[&str], &str)] = &[
        (&["--unknown"], "unknown argument"),
        (&["--print"], "need a session ID"),
        (&["--open", "--fuzzy"], "need a session ID"),
        (&["--list", "--list"], "choose one"),
        (&["--list", "--print", "id"], "choose one"),
        (&["--print", "id", "--open", "id"], "choose one"),
        (&["--picker-header", "extra"], "unknown argument"),
        (&["--picker-preview", "path", "exact"], "unknown argument"),
        (
            &["--picker-preview", "path", "other", "query"],
            "unknown argument",
        ),
    ];
    for &(args, message) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(args)
            .env_remove("HOME")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(message), "{args:?}: {error}");
        assert!(!error.contains("HOME"), "{args:?}: {error}");
    }
}

#[test]
fn help_takes_precedence_over_invalid_public_arguments() {
    let cases: &[&[&str]] = &[
        &["--help"],
        &["--unknown", "-h"],
        &["--print", "--help"],
        &["--list", "--open", "id", "--help"],
    ];
    for &args in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(args)
            .env_remove("HOME")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("Usage: session-finder"));
    }
}

#[test]
fn print_reports_missing_and_ambiguous_ids_without_selecting_a_session() {
    let root = std::env::temp_dir().join(format!("session-lookup-{}", std::process::id()));
    fs::create_dir_all(root.join("claude")).unwrap();
    fs::create_dir_all(root.join("codex")).unwrap();
    let id = "123e4567-e89b-12d3-a456-426614174000";
    fs::write(root.join("claude").join(format!("{id}.jsonl")),
        serde_json::json!({"type": "user", "sessionId": id, "cwd": "/work", "message": {"content": "Claude request"}}).to_string()
    ).unwrap();
    fs::write(
        root.join("codex/rollout-fixture.jsonl"),
        serde_json::json!({"type": "session_meta", "payload": {"id": id, "cwd": "/work"}})
            .to_string(),
    )
    .unwrap();
    for (query, expected) in [(id, "found 2"), ("missing", "found 0")] {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(["--print", query])
            .env("SESSION_FINDER_CLAUDE_ROOT", root.join("claude"))
            .env("SESSION_FINDER_CODEX_ROOT", root.join("codex"))
            .env("SESSION_FINDER_CODEX_ARCHIVE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CURSOR_DB", root.join("missing.db"))
            .env("SESSION_FINDER_CACHE", root.join("index.json"))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr).unwrap().contains(expected));
    }
    fs::remove_dir_all(root).unwrap();
}
