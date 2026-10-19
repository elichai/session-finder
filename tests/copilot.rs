use std::fs;
use std::process::Command;

#[test]
fn copilot_history_is_searchable_resumable_and_refreshed_automatically() {
    let root = std::env::temp_dir().join(format!("session-copilot-{}", std::process::id()));
    let state = root.join("copilot/session-state");
    let id = "work-task_123";
    let dir = state.join(id);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("events.jsonl");
    let events = concat!(
        "{\"type\":\"session.start\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"data\":{\"sessionId\":\"work-task_123\",\"context\":{\"cwd\":\"/old\"}}}\n",
        "{\"type\":\"user.message\",\"data\":{\"source\":\"skill-pdf\",\"content\":\"hidden skill injection\"}}\n",
        "{\"type\":\"user.message\",\"timestamp\":\"2025-01-02T03:04:05Z\",\"data\":{\"source\":\"user\",\"content\":\"find the café bug\",\"transformedContent\":\"hidden wrapper\"}}\n",
        "not-json\n",
        "{\"type\":\"assistant.message\",\"data\":{\"content\":\"answer with a \\\"quote\\\"\"}}\n",
        "{\"type\":\"tool.execution_complete\",\"data\":{\"content\":\"hidden tool output\"}}\n",
        "{\"type\":\"assistant.reasoning\",\"data\":{\"content\":\"hidden reasoning\"}}\n",
        "{\"type\":\"assistant.message\",\"agentId\":\"child\",\"data\":{\"content\":\"hidden subagent\"}}\n",
        "{\"type\":\"user.message\",\"data\":{\"source\":\"agent-reviewer\",\"content\":\"hidden inter-agent prompt\"}}\n",
        "{\"type\":\"user.message\",\"data\":{\"source\":\"system\",\"content\":\"hidden system context\"}}\n",
        "{\"type\":\"user.message\",\"data\":{\"isAutopilotContinuation\":true,\"content\":\"hidden autopilot\"}}\n",
        "{\"type\":\"assistant.message\",\"data\":{\"parentToolCallId\":\"child\",\"content\":\"hidden legacy subagent\"}}\n",
        "{\"type\":\"session.context_changed\",\"timestamp\":\"2025-01-02T03:04:06Z\",\"data\":{\"cwd\":\"/tmp/O'Brien; echo bad\"}}\n"
    );
    fs::write(&file, events).unwrap();
    let run = |args: &[&str], override_root: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command
            .args(args)
            .env("HOME", &root)
            .env("COPILOT_HOME", root.join("copilot"))
            .env_remove("SESSION_FINDER_COPILOT_ROOT")
            .env("SESSION_FINDER_CLAUDE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CODEX_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CODEX_ARCHIVE_ROOT", root.join("missing"))
            .env("SESSION_FINDER_CURSOR_DB", root.join("missing.db"))
            .env("SESSION_FINDER_CACHE", root.join("index.json"));
        if override_root {
            command
                .env("COPILOT_HOME", root.join("wrong"))
                .env("SESSION_FINDER_COPILOT_ROOT", &state);
        }
        command.output().unwrap()
    };
    let output = run(&["--list"], false);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "1735787046\tCopilot\twork-task_123\t/tmp/O'Brien; echo bad\tfind the café bug\n"
    );
    let output = run(&["--print", id], true);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "cd -- '/tmp/O'\\''Brien; echo bad' && copilot --resume='work-task_123'\n"
    );
    let rows = run(&["--picker-rows"], false);
    let rows = String::from_utf8(rows.stdout).unwrap();
    assert!(rows.starts_with("Copilot:work-task_123\t"));
    assert!(rows.contains("find the café bug"));
    assert!(rows.contains("answer with a \"quote\""));
    assert!(!rows.contains("hidden"));
    let output = run(&["--open", id], false);
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("Copilot CLI"));

    fs::write(&file, [events, "{\"type\":\"user.message\",\"timestamp\":\"2025-01-03T00:00:00Z\",\"data\":{\"content\":\"new request\"}}\n"].concat()).unwrap();
    let output = run(&["--picker-rows"], false);
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("new request"));
    fs::remove_file(&file).unwrap();
    assert!(run(&["--list"], false).stdout.is_empty());
    fs::remove_dir_all(root).unwrap();
}
