use std::fs;
use std::path::Path;
use std::process::Command;

const LOCAL: &str = "11111111-2222-4333-8444-555555555555";
const REMOTE: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn transcript(storage: &Path, id: &str, text: &str) -> std::path::PathBuf {
    let dir = storage.join("GitHub.copilot-chat/transcripts");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{id}.jsonl"));
    fs::write(&file, text).unwrap();
    file
}

fn events(id: &str, request: &str) -> String {
    [
        format!("{{\"type\":\"session.start\",\"data\":{{\"sessionId\":\"{id}\",\"version\":1,\"producer\":\"copilot-agent\",\"copilotVersion\":\"0.30.0\",\"vscodeVersion\":\"1.105.0\",\"startTime\":\"2025-01-01T00:00:00.000Z\"}},\"id\":\"e1\",\"timestamp\":\"2025-01-01T00:00:00.000Z\",\"parentId\":null}}\n"),
        format!("{{\"type\":\"user.message\",\"data\":{{\"content\":\"{request}\",\"attachments\":[]}},\"id\":\"e2\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"parentId\":\"e1\"}}\n"),
        "{\"type\":\"assistant.turn_start\",\"data\":{\"turnId\":\"0\"},\"id\":\"e3\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"parentId\":\"e2\"}\n".into(),
        "{\"type\":\"assistant.message\",\"data\":{\"messageId\":\"m\",\"content\":\"answer with a \\\"quote\\\"\",\"toolRequests\":[],\"reasoningText\":\"hidden reasoning\"},\"id\":\"e4\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"parentId\":\"e3\"}\n".into(),
        "{\"type\":\"tool.execution_complete\",\"data\":{\"content\":\"hidden tool output\"},\"id\":\"e5\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"parentId\":\"e4\"}\n".into(),
    ]
    .concat()
}

#[test]
fn vscode_copilot_chat_history_is_searchable_with_and_without_workspace_metadata() {
    let root = std::env::temp_dir().join(format!("session-vscode-{}", std::process::id()));
    let storage = root.join(".vscode-server/data/User/workspaceStorage");
    let local = storage.join("0123abcd");
    fs::create_dir_all(&local).unwrap();
    fs::write(
        local.join("workspace.json"),
        r#"{"folder":"file:///tmp/O'Brien%3B%20echo%20bad"}"#,
    )
    .unwrap();
    let local_file = transcript(&local, LOCAL, &events(LOCAL, "find the café bug"));
    // Remote-SSH servers keep no workspace.json, so the folder is unknown.
    transcript(
        &storage.join("4567cdef"),
        REMOTE,
        &events(REMOTE, "remote request"),
    );
    // Only transcripts with UUID names are sessions; other Copilot Chat logs are not.
    transcript(
        &storage.join("4567cdef"),
        "not-a-uuid",
        &events("not-a-uuid", "hidden"),
    );
    let debug = storage.join(format!("4567cdef/GitHub.copilot-chat/debug-logs/{REMOTE}"));
    fs::create_dir_all(&debug).unwrap();
    fs::write(debug.join("main.jsonl"), events(REMOTE, "hidden debug log")).unwrap();

    let run = |args: &[&str], override_root: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command
            .args(args)
            .env("HOME", &root)
            .env_remove("SESSION_FINDER_VSCODE_ROOT")
            .env("SESSION_FINDER_CLAUDE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CODEX_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CODEX_ARCHIVE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_COPILOT_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CURSOR_DB", root.join("missing.db"))
            .env("SESSION_FINDER_CACHE", root.join("index.json"));
        if override_root {
            command.env("HOME", root.join("wrong")).env(
                "SESSION_FINDER_VSCODE_ROOT",
                format!("{}:{}", root.join("missing").display(), storage.display()),
            );
        }
        command.output().unwrap()
    };

    let output = run(&["--list"], false);
    assert!(output.status.success());
    // Equal timestamps sort by ID.
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "1735787045\tVSCode\t{LOCAL}\t/tmp/O'Brien; echo bad\tfind the café bug\n\
             1735787045\tVSCode\t{REMOTE}\t\tremote request\n"
        )
    );

    let output = run(&["--print", LOCAL], true);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "code '/tmp/O'\\''Brien; echo bad'\n"
    );
    let output = run(&["--print", REMOTE], true);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("# VS Code chat {REMOTE}: reopen it from VS Code's Chat sessions view\n")
    );

    let rows = String::from_utf8(run(&["--picker-rows"], false).stdout).unwrap();
    assert!(rows.contains(&format!("VSCode:{LOCAL}\t")));
    assert!(rows.contains(&format!("VSCode:{REMOTE}\t")));
    assert!(rows.contains("answer with a \"quote\""));
    assert!(!rows.contains("hidden"));

    let output = run(&["--open", LOCAL], false);
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("VS Code chat"));

    fs::write(
        &local_file,
        [
            events(LOCAL, "find the café bug"),
            "{\"type\":\"user.message\",\"data\":{\"content\":\"new request\",\"attachments\":[]},\"id\":\"e6\",\"timestamp\":\"2025-01-03T00:00:00.000Z\",\"parentId\":\"e5\"}\n".into(),
        ]
        .concat(),
    )
    .unwrap();
    let rows = String::from_utf8(run(&["--picker-rows"], false).stdout).unwrap();
    assert!(rows.starts_with(&format!("VSCode:{LOCAL}\t")));
    assert!(rows.contains("new request"));
    fs::remove_file(&local_file).unwrap();
    let list = String::from_utf8(run(&["--list"], false).stdout).unwrap();
    assert!(!list.contains(LOCAL));
    assert!(list.contains(REMOTE));
    fs::remove_dir_all(root).unwrap();
}
