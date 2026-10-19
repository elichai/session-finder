use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const CLAUDE: &str = "11111111-2222-4333-8444-555555555555";
const CODEX: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const ARCHIVED: &str = "123e4567-e89b-12d3-a456-426614174000";

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "extension-history-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("work")).unwrap();
        Self(root)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        // No inherited provider paths or editor executable can reach real history/apps.
        command
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", "/usr/bin:/bin")
            .env("CLAUDE_CONFIG_DIR", self.0.join("claude-config"))
            .env("CODEX_HOME", self.0.join("codex-home"))
            .env("SESSION_FINDER_CLAUDE_ROOT", self.0.join("claude"))
            .env("SESSION_FINDER_CODEX_ROOT", self.0.join("codex"))
            .env("SESSION_FINDER_CODEX_ARCHIVE_ROOT", self.0.join("archive"))
            .env("SESSION_FINDER_COPILOT_ROOT", self.0.join("copilot"))
            .env("SESSION_FINDER_VSCODE_ROOT", self.0.join("vscode"))
            .env("SESSION_FINDER_CURSOR_DB", self.0.join("missing.db"))
            .env("SESSION_FINDER_CACHE", self.0.join("index.json"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn claude(&self, root: &Path, id: &str) {
        fs::create_dir_all(root).unwrap();
        let record = json!({
            "type": "user", "sessionId": id, "cwd": self.0.join("work"),
            "timestamp": "2025-01-02T03:04:05Z", "message": {"content": "extension request"}
        });
        fs::write(root.join(format!("{id}.jsonl")), format!("{record}\n")).unwrap();
    }

    fn codex(&self, root: &Path, id: &str) {
        fs::create_dir_all(root).unwrap();
        let records = [
            json!({"type":"session_meta", "payload":{
                "id":id, "cwd":self.0.join("work"), "originator":"codex_vscode",
                "source":"vscode", "thread_source":"user"
            }}),
            json!({"type":"response_item", "timestamp":"2025-01-02T03:04:05Z", "payload":{
                "type":"message", "role":"user", "content":[{"type":"input_text","text":"codex extension request"}]
            }}),
            json!({"type":"response_item", "payload":{
                "type":"message", "role":"assistant", "content":[{"type":"output_text","text":"extension answer"}]
            }}),
            json!({"type":"event_msg", "payload":{"type":"user_message", "message":"codex extension request"}}),
        ];
        let text = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>();
        fs::write(root.join(format!("rollout-{id}.jsonl")), text).unwrap();
    }

    fn sessions(&self) {
        self.claude(&self.0.join("claude"), CLAUDE);
        self.codex(&self.0.join("codex"), CODEX);
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn custom_config_homes_find_extension_history_without_duplicate_codex_sessions() {
    let fixture = Fixture::new("homes");
    fixture.claude(&fixture.0.join("claude-config/projects/project"), CLAUDE);
    fixture.codex(&fixture.0.join("codex-home/sessions/2025/01/02"), CODEX);
    fixture.codex(&fixture.0.join("codex-home/archived_sessions"), ARCHIVED);
    let mut command = fixture.command();
    for variable in [
        "SESSION_FINDER_CLAUDE_ROOT",
        "SESSION_FINDER_CODEX_ROOT",
        "SESSION_FINDER_CODEX_ARCHIVE_ROOT",
    ] {
        command.env_remove(variable);
    }
    let list = stdout(&command.arg("--list").output().unwrap());
    assert_eq!(list.lines().count(), 3, "{list}");
    for (source, id) in [("Claude", CLAUDE), ("Codex", CODEX), ("Codex*", ARCHIVED)] {
        assert_eq!(
            list.matches(&format!("\t{source}\t{id}\t")).count(),
            1,
            "{list}"
        );
    }
    let cache: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("index.json")).unwrap()).unwrap();
    let codex = cache["entries"]
        .as_object()
        .unwrap()
        .values()
        .find_map(|entry| (entry["session"]["id"] == CODEX).then_some(&entry["session"]))
        .unwrap();
    assert_eq!(codex["source"], "Codex");
    assert_eq!(codex["search_text"], "codex extension request");
    assert_eq!(codex["assistant_text"], "extension answer");
}

#[test]
fn explicit_history_roots_override_both_custom_homes() {
    let fixture = Fixture::new("overrides");
    fixture.sessions();
    fixture.codex(&fixture.0.join("archive"), ARCHIVED);
    fixture.claude(&fixture.0.join("claude-config/projects"), CODEX);
    fixture.codex(&fixture.0.join("codex-home/sessions"), CLAUDE);
    fixture.codex(&fixture.0.join("codex-home/archived_sessions"), CLAUDE);
    let list = stdout(&fixture.run(&["--list"]));
    assert_eq!(list.lines().count(), 3, "{list}");
    assert!(list.contains(&format!("\tClaude\t{CLAUDE}\t")));
    assert!(list.contains(&format!("\tCodex\t{CODEX}\t")));
    assert!(list.contains(&format!("\tCodex*\t{ARCHIVED}\t")));
}

#[test]
fn extension_print_modes_use_existing_session_links_and_warn_about_workspace() {
    let fixture = Fixture::new("print");
    fixture.sessions();
    for (flag, editor, scheme) in [
        ("--vscode", "code", "vscode"),
        ("--vscode-insiders", "code-insiders", "vscode-insiders"),
    ] {
        for (id, route) in [
            (
                CLAUDE,
                format!("anthropic.claude-code/open?session={CLAUDE}"),
            ),
            (CODEX, format!("openai.chatgpt/local/{CODEX}")),
        ] {
            let output = fixture.run(&[flag, "--print", id]);
            assert_eq!(
                stdout(&output),
                format!("{editor} --open-url '{scheme}://{route}'\n")
            );
            let warning = String::from_utf8(output.stderr).unwrap().to_lowercase();
            assert!(
                warning.contains("focus") && warning.contains("workspace"),
                "{warning}"
            );
            if id == CLAUDE {
                assert!(warning.contains("new chat"), "{warning}");
            }
        }
    }
}

#[test]
fn default_print_keeps_terminal_resume_commands() {
    let fixture = Fixture::new("terminal");
    fixture.sessions();
    let cwd = fixture.0.join("work");
    assert_eq!(
        stdout(&fixture.run(&["--print", CLAUDE])),
        format!("cd -- '{}' && claude --resume '{CLAUDE}'\n", cwd.display())
    );
    assert_eq!(
        stdout(&fixture.run(&["--print", CODEX])),
        format!("codex resume '{CODEX}' -C '{}'\n", cwd.display())
    );
}

#[test]
fn extension_open_passes_exact_args_to_editor_and_reports_editor_failure() {
    let fixture = Fixture::new("open");
    fixture.sessions();
    let bin = fixture.0.join("bin");
    let capture = fixture.0.join("editor-args");
    fs::create_dir(&bin).unwrap();
    for editor in ["code", "code-insiders"] {
        let executable = bin.join(editor);
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$EDITOR_ARGS\"\nexit \"$EDITOR_STATUS\"\n",
        )
        .unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (flag, scheme, id, route, status) in [
        ("--vscode", "vscode", CODEX, "openai.chatgpt/local/", "0"),
        (
            "--vscode-insiders",
            "vscode-insiders",
            CLAUDE,
            "anthropic.claude-code/open?session=",
            "0",
        ),
        ("--vscode", "vscode", CODEX, "openai.chatgpt/local/", "9"),
    ] {
        let output = fixture
            .command()
            .args([flag, "--open", id])
            .env("PATH", &bin)
            .env("EDITOR_ARGS", &capture)
            .env("EDITOR_STATUS", status)
            .output()
            .unwrap();
        assert_eq!(
            fs::read_to_string(&capture).unwrap(),
            format!("--open-url\n{scheme}://{route}{id}\n")
        );
        assert_eq!(
            output.status.success(),
            status == "0",
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.to_lowercase().contains("workspace"), "{stderr}");
        if status != "0" {
            assert!(stderr.contains('9'), "{stderr}");
        }
    }
}

#[test]
fn extension_modes_reject_conflicting_flags_and_unsupported_providers() {
    let fixture = Fixture::new("errors");
    let output = fixture.run(&["--vscode", "--vscode-insiders", "--list"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--vscode") && !stderr.contains("unknown argument"),
        "{stderr}"
    );

    let copilot = fixture.0.join("copilot/copilot-task");
    fs::create_dir_all(&copilot).unwrap();
    fs::write(copilot.join("events.jsonl"), concat!(
        "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-task\",\"context\":{\"cwd\":\"/work\"}}}\n",
        "{\"type\":\"user.message\",\"data\":{\"content\":\"unsupported provider\"}}\n"
    )).unwrap();
    for flag in ["--vscode", "--vscode-insiders"] {
        let output = fixture.run(&[flag, "--print", "copilot-task"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            !stderr.contains("unknown argument") && !stderr.contains("found 0"),
            "{stderr}"
        );
    }
}

#[test]
fn extension_picker_filters_initial_rows_and_refresh_without_changing_default_rows() {
    let fixture = Fixture::new("picker-filter");
    fixture.sessions();
    let copilot = fixture.0.join("copilot/copilot-task");
    fs::create_dir_all(&copilot).unwrap();
    fs::write(copilot.join("events.jsonl"), concat!(
        "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-task\",\"context\":{\"cwd\":\"/work\"}}}\n",
        "{\"type\":\"user.message\",\"data\":{\"content\":\"unsupported provider\"}}\n"
    )).unwrap();
    let default_rows = stdout(&fixture.run(&["--picker-rows"]));
    assert_eq!(default_rows.lines().count(), 3);
    assert!(default_rows.contains("Copilot:copilot-task\t"));

    let extension_rows = stdout(&fixture.run(&["--picker-extension-rows"]));
    assert_eq!(extension_rows.lines().count(), 2);
    for identity in [format!("Claude:{CLAUDE}\t"), format!("Codex:{CODEX}\t")] {
        assert!(extension_rows.contains(&identity), "{extension_rows}");
    }
    assert!(!extension_rows.contains("Copilot:"));

    let bin = fixture.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let picker = bin.join("fzf");
    fs::write(
        &picker,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$PICKER_ARGS\"\n/bin/cat > \"$PICKER_ROWS\"\nexit 130\n",
    )
    .unwrap();
    fs::set_permissions(picker, fs::Permissions::from_mode(0o700)).unwrap();
    let args_path = fixture.0.join("picker-args");
    let rows_path = fixture.0.join("picker-rows");
    for flag in ["--vscode", "--vscode-insiders"] {
        let output = fixture
            .command()
            .arg(flag)
            .env("PATH", &bin)
            .env("PICKER_ARGS", &args_path)
            .env("PICKER_ROWS", &rows_path)
            .output()
            .unwrap();
        stdout(&output);
        assert_eq!(fs::read_to_string(&rows_path).unwrap(), extension_rows);
        let args = fs::read_to_string(&args_path).unwrap();
        assert!(args.contains("--picker-extension-rows"), "{args}");
        assert!(!args.contains(" --picker-rows"), "{args}");
        let prompt = if flag == "--vscode" {
            "--prompt=VS Code> "
        } else {
            "--prompt=VS Code Insiders> "
        };
        assert!(args.lines().any(|arg| arg == prompt), "{args}");
    }
}
