use std::fs;
use std::process::Command;

#[test]
fn preview_uses_the_query_and_only_includes_assistant_when_enabled() {
    let path = std::env::temp_dir().join(format!("session-preview-{}.txt", std::process::id()));
    fs::write(&path, "Claude:123e4567-e89b-12d3-a456-426614174000\tnow Claude /work\topening\topening then a timing attack discussion\t/work\t123e4567-e89b-12d3-a456-426614174000\tsecret assistant answer\n").unwrap();
    let preview = |query: &str, fields: &str, fuzzy: bool| {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args([
                "--picker-preview",
                path.to_str().unwrap(),
                if fuzzy { "fuzzy" } else { "exact" },
                query,
            ])
            .env("FZF_QUERY", query)
            .env("FZF_WITH_NTH", fields)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let output = preview("timing attack", "2..6", false);
    assert!(output.contains("User"));
    assert!(
        output.contains("\x1b[1;33mtiming attack\x1b[0m"),
        "{output}"
    );
    assert!(!output.contains("secret"));
    assert!(!preview("secret", "2..6", false).contains("secret"));
    let output = preview("secret", "2..7", false);
    assert!(output.contains("Assistant"));
    assert!(output.contains("\x1b[1;33msecret\x1b[0m"));
    assert!(preview("tmng", "2..6", true).contains("\x1b[1;33mt"));
    fs::remove_file(path).unwrap();
}

#[test]
fn refresh_rows_discover_changed_new_and_deleted_sessions() {
    let root = std::env::temp_dir().join(format!("session-refresh-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let file = root.join("123e4567-e89b-12d3-a456-426614174000.jsonl");
    let write = |text: &str| {
        fs::write(
            &file,
            serde_json::json!({
                "type": "user", "sessionId": "123e4567-e89b-12d3-a456-426614174000",
                "cwd": "/work", "message": {"content": text}
            })
            .to_string(),
        )
        .unwrap()
    };
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(args)
            .env("SESSION_FINDER_CLAUDE_ROOT", &root)
            .env("SESSION_FINDER_CODEX_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CODEX_ARCHIVE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_COPILOT_ROOT", root.join("missing"))
            .env("SESSION_FINDER_VSCODE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CURSOR_DB", root.join("missing.db"))
            .env("SESSION_FINDER_CACHE", root.join("index.json"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let rows = || run(&["--picker-rows"]);
    assert!(rows().is_empty());
    write("first request");
    let before = rows();
    assert!(before.contains("first request"));
    let listed = run(&["--fuzzy", "--list", "--rebuild"]);
    assert_eq!(listed.lines().count(), 1);
    assert!(listed.contains("Claude\t123e4567-e89b-12d3-a456-426614174000\t/work\tfirst request"));
    assert_eq!(
        run(&["--print", "123e4567-e89b-12d3-a456-426614174000", "--fuzzy"]),
        "cd -- '/work' && claude --resume '123e4567-e89b-12d3-a456-426614174000'\n"
    );
    write("changed request with more text");
    let after = rows();
    assert!(after.contains("changed request with more text"));
    assert!(!after.contains("first request"));
    assert_eq!(before.split('\t').next(), after.split('\t').next());
    fs::remove_file(&file).unwrap();
    assert!(rows().is_empty());
    fs::remove_dir_all(root).unwrap();
}
