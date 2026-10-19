use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const ID: &str = "11111111-2222-4333-8444-555555555555";

fn root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(
        [
            "session-vscode-",
            label,
            "-",
            &std::process::id().to_string(),
        ]
        .concat(),
    );
    fs::create_dir_all(&root).unwrap();
    root
}

fn transcript(workspace: &Path) {
    let dir = workspace.join("GitHub.copilot-chat/transcripts");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join([ID, ".jsonl"].concat()),
        [
            serde_json::json!({"type":"session.start","data":{"sessionId":ID}}).to_string(),
            serde_json::json!({"type":"user.message","data":{"content":"Copilot request"}})
                .to_string(),
        ]
        .join("\n"),
    )
    .unwrap();
}

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
        .args(args)
        .env("HOME", root)
        .env_remove("SESSION_FINDER_VSCODE_ROOT")
        .env("SESSION_FINDER_CLAUDE_ROOT", root.join("missing"))
        .env("SESSION_FINDER_CODEX_ROOT", root.join("missing"))
        .env("SESSION_FINDER_CODEX_ARCHIVE_ROOT", root.join("missing"))
        .env("SESSION_FINDER_COPILOT_ROOT", root.join("missing"))
        .env("SESSION_FINDER_CURSOR_DB", root.join("missing.db"))
        .env("SESSION_FINDER_CACHE", root.join("index.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn workspace_metadata_refreshes_without_reparsing_the_transcript() {
    let root = root("metadata");
    let workspace = root.join(".config/Code/User/workspaceStorage/workspace");
    transcript(&workspace);
    let metadata = workspace.join("workspace.json");
    fs::write(&metadata, r#"{"folder":"file:///tmp/before"}"#).unwrap();
    assert_eq!(
        String::from_utf8(run(&root, &["--print", ID]).stdout).unwrap(),
        "code '/tmp/before'\n"
    );
    fs::write(&metadata, r#"{"folder":"file:///tmp/after"}"#).unwrap();
    assert_eq!(
        String::from_utf8(run(&root, &["--print", ID]).stdout).unwrap(),
        "code '/tmp/after'\n"
    );
    fs::remove_file(&metadata).unwrap();
    let unknown = String::from_utf8(run(&root, &["--print", ID]).stdout).unwrap();
    assert!(unknown.starts_with('#'));
    assert!(!unknown.contains("/tmp/after"));
    fs::write(&metadata, r#"{"folder":"file:///tmp/created"}"#).unwrap();
    assert_eq!(
        String::from_utf8(run(&root, &["--print", ID]).stdout).unwrap(),
        "code '/tmp/created'\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn multi_root_workspace_keeps_its_file_as_the_opening_target() {
    let root = root("multi-root");
    let workspace = root.join(".config/Code/User/workspaceStorage/workspace");
    transcript(&workspace);
    let target = root.join("team.code-workspace");
    fs::write(
        &target,
        r#"{"folders":[{"path":"first"},{"path":"second"}]}"#,
    )
    .unwrap();
    let uri = ["file://", target.to_str().unwrap()].concat();
    fs::write(
        workspace.join("workspace.json"),
        serde_json::json!({"workspace":uri}).to_string(),
    )
    .unwrap();
    let output = run(&root, &["--print", ID]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        ["code '", target.to_str().unwrap(), "'\n"].concat()
    );
    assert!(!String::from_utf8(output.stderr)
        .unwrap()
        .contains("missing"));
    let list = String::from_utf8(run(&root, &["--list"]).stdout).unwrap();
    assert!(list.contains([ID, "\t\tCopilot request"].concat().as_str()));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn insiders_sessions_open_in_insiders() {
    let root = root("insiders");
    let workspace =
        root.join("Library/Application Support/Code - Insiders/User/workspaceStorage/workspace");
    transcript(&workspace);
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///tmp/insiders"}"#,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(run(&root, &["--print", ID]).stdout).unwrap(),
        "code-insiders '/tmp/insiders'\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_chat_history_supports_multiple_extensions_and_deduplicates_copilot() {
    let root = root("native");
    let workspace = root.join(".config/Code/User/workspaceStorage/workspace");
    transcript(&workspace);
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///tmp/native"}"#,
    )
    .unwrap();
    let chats = workspace.join("chatSessions");
    fs::create_dir_all(&chats).unwrap();
    fs::write(chats.join([ID, ".json"].concat()), serde_json::json!({
        "version":3,"creationDate":1700000000000i64,"customTitle":"Saved Copilot discussion",
        "requests":[{"message":{"text":"Copilot request"},"agent":{"id":"copilot","extensionId":{"value":"GitHub.copilot-chat"}},"response":[{"value":"Saved native answer"}]}]
    }).to_string()).unwrap();
    let other = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    fs::write(chats.join([other, ".jsonl"].concat()), [
        serde_json::json!({"kind":0,"v":{"version":3,"creationDate":1700000000000i64,"requests":[]}}).to_string(),
        serde_json::json!({"kind":2,"k":["requests"],"v":[{"message":{"text":"Another extension request"},"agent":{"id":"vendor.helper","extensionId":{"value":"Vendor.Helper"}},"response":[{"value":"Vendor answer"},{"kind":"thinking","value":"private thought"},{"kind":"toolInvocationSerialized","value":"private tool output"}]}]}).to_string(),
    ].join("\n")).unwrap();
    let list = String::from_utf8(run(&root, &["--list"]).stdout).unwrap();
    assert_eq!(list.lines().count(), 2, "{list}");
    assert!(list.contains("Copilot request"));
    assert!(list.contains("Another extension request"));
    let rows = String::from_utf8(run(&root, &["--picker-rows"]).stdout).unwrap();
    assert!(rows.contains("GitHub.copilot-chat"));
    assert!(rows.contains("Vendor.Helper"));
    assert!(rows.contains("Saved Copilot discussion"));
    assert!(rows.contains("Saved native answer"));
    assert!(rows.contains("Vendor answer"));
    assert!(!rows.contains("private"));
    assert_eq!(
        String::from_utf8(run(&root, &["--print", other]).stdout).unwrap(),
        "code '/tmp/native'\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_journal_wins_over_a_legacy_snapshot_after_a_rollback() {
    let root = root("rollback");
    let chats = root.join(".config/Code/User/workspaceStorage/workspace/chatSessions");
    fs::create_dir_all(&chats).unwrap();
    fs::write(chats.join([ID,".json"].concat()), serde_json::json!({"requests":[{"timestamp":1700000005000i64,"message":"Stale deleted prompt"}]}).to_string()).unwrap();
    fs::write(chats.join([ID,".jsonl"].concat()), serde_json::json!({"kind":0,"v":{"requests":[{"timestamp":1700000001000i64,"message":"Current saved prompt"}]}}).to_string()).unwrap();
    let rows = String::from_utf8(run(&root, &["--picker-rows"]).stdout).unwrap();
    assert!(rows.contains("Current saved prompt"), "{rows}");
    assert!(!rows.contains("Stale deleted prompt"), "{rows}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_window_sessions_are_discovered_without_an_invented_workspace() {
    let root = root("empty-window");
    let chats = root.join(".config/Code/User/globalStorage/emptyWindowChatSessions");
    fs::create_dir_all(&chats).unwrap();
    fs::write(chats.join([ID,".jsonl"].concat()), serde_json::json!({"kind":0,"v":{"creationDate":1700000000000i64,"requests":[{"message":"Chat without a workspace","response":[{"value":"An answer"}]}]}}).to_string()).unwrap();
    let list = String::from_utf8(run(&root, &["--list"]).stdout).unwrap();
    assert!(
        list.contains([ID, "\t\tChat without a workspace"].concat().as_str()),
        "{list}"
    );
    let command = String::from_utf8(run(&root, &["--print", ID]).stdout).unwrap();
    assert!(command.starts_with('#'));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_native_sessions_do_not_restore_hook_or_legacy_text() {
    let root = root("native-empty");
    let workspace = root.join(".config/Code/User/workspaceStorage/workspace");
    transcript(&workspace);
    let chats = workspace.join("chatSessions");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join([ID, ".json"].concat()),
        serde_json::json!({"requests":[{"message":"Old native snapshot"}]}).to_string(),
    )
    .unwrap();
    let journal = chats.join([ID, ".jsonl"].concat());
    for requests in [
        serde_json::json!([]),
        serde_json::json!([{"message":"Hidden request","hiddenFromTranscript":true,"response":"Hidden answer"}]),
    ] {
        fs::write(
            &journal,
            serde_json::json!({"kind":0,"v":{"requests":requests}}).to_string(),
        )
        .unwrap();
        let list = String::from_utf8(run(&root, &["--list"]).stdout).unwrap();
        assert!(list.is_empty(), "{list}");
    }
    fs::write(&journal, "corrupt journal\n").unwrap();
    let list = String::from_utf8(run(&root, &["--list"]).stdout).unwrap();
    assert!(list.contains("Old native snapshot"));
    assert!(!list.contains("Copilot request"));
    fs::remove_dir_all(root).unwrap();
}
