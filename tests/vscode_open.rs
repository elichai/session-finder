use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

const ID: &str = "11111111-2222-4333-8444-555555555555";
const TOKEN: &str = "MTExMTExMTEtMjIyMi00MzMzLTg0NDQtNTU1NTU1NTU1NTU1";
const STORE: &str = ".config/Code/User/workspaceStorage/hash";

struct Fixture {
    root: PathBuf,
    storage: PathBuf,
    custom: bool,
}

impl Fixture {
    fn new(label: &str, store: &str, custom: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "session-vscode-open-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage = root.join(store);
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        let script = r#"#!/bin/sh
printf '%s\t' "${0##*/}" "$@" >> "$CODE_LOG"
printf '\n' >> "$CODE_LOG"
for arg do
    case "$arg" in
        --status)
            if [ -e "$EDITOR_READY" ]; then
                printf 'Version: 1.105.0\nProcess Argv: --user-data-dir test\n'
            else
                printf 'Warning: Code is not running.\n'
            fi
            exit 0 ;;
        --new-window|--open-url)
            if [ "$CODE_FAIL" = 1 ]; then printf 'editor failed\n' >&2; exit 7; fi
            : > "$EDITOR_READY"
            exit 0 ;;
    esac
done
exit 9
"#;
        for editor in ["code", "code-insiders"] {
            let file = root.join("bin").join(editor);
            fs::write(&file, script).unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            root,
            storage,
            custom,
        }
    }

    fn command(&self, executable: &str) -> Command {
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", self.root.join("bin"))
            .env("CODE_LOG", self.root.join("calls"))
            .env("EDITOR_READY", self.root.join("ready"))
            .env("SESSION_FINDER_CACHE", self.root.join("index.json"));
        for name in [
            "CLAUDE_ROOT",
            "CODEX_ROOT",
            "CODEX_ARCHIVE_ROOT",
            "COPILOT_ROOT",
            "CURSOR_DB",
        ] {
            command.env(format!("SESSION_FINDER_{name}"), self.root.join("missing"));
        }
        if self.custom {
            command.env("SESSION_FINDER_VSCODE_ROOT", self.storage.parent().unwrap());
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_session-finder"))
            .args(args)
            .output()
            .unwrap()
    }

    fn native(&self, journal: bool) {
        let chats = self.storage.join("chatSessions");
        fs::create_dir_all(&chats).unwrap();
        let session = serde_json::json!({"version":3,"creationDate":1700000000000i64,"requests":[{"message":{"text":"Saved request"},"response":[{"value":"Saved answer"}]}]});
        let value = if journal {
            serde_json::json!({"kind":0,"v":session})
        } else {
            session
        };
        fs::write(
            chats.join(format!("{ID}.{}", if journal { "jsonl" } else { "json" })),
            value.to_string(),
        )
        .unwrap();
    }

    fn metadata(&self, workspace: bool) -> PathBuf {
        let suffix = if workspace {
            "café #? O'Brien.code-workspace"
        } else {
            "café #? O'Brien"
        };
        let target = self.root.join(suffix);
        if workspace {
            fs::write(
                &target,
                r#"{"folders":[{"path":"first"},{"path":"second"}]}"#,
            )
            .unwrap();
        } else {
            fs::create_dir_all(&target).unwrap();
        }
        let uri = format!(
            "file://{}/caf%C3%A9%20%23%3F%20O%27Brien{}",
            self.root.display(),
            if workspace { ".code-workspace" } else { "" }
        );
        let metadata = if workspace {
            serde_json::json!({"workspace":uri})
        } else {
            serde_json::json!({"folder":uri})
        };
        fs::write(self.storage.join("workspace.json"), metadata.to_string()).unwrap();
        target
    }

    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                line.trim_end_matches('\t')
                    .split('\t')
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    fn assert_opened(&self, target: &std::path::Path, editor_scheme: &str, cold: bool) {
        let calls = self.calls();
        assert_eq!(calls.len(), if cold { 4 } else { 2 }, "{calls:?}");
        let data = self.storage.ancestors().nth(3).unwrap().to_str().unwrap();
        for call in &calls {
            assert_eq!(
                call[0],
                if editor_scheme == "vscode" {
                    "code"
                } else {
                    "code-insiders"
                }
            );
            let option = call
                .iter()
                .position(|arg| arg == "--user-data-dir")
                .expect("every editor call must select the source store");
            assert_eq!(call[option + 1], data, "{call:?}");
        }
        assert!(calls[0].iter().any(|arg| arg == "--status"));
        if cold {
            assert!(calls[1].iter().any(|arg| arg == "--new-window"));
            assert!(calls[1].iter().any(|arg| arg == target.to_str().unwrap()));
            assert!(calls[2].iter().any(|arg| arg == "--status"));
        }
        let suffix = if target.extension().is_some() {
            ".code-workspace"
        } else {
            ""
        };
        let expected = format!("{editor_scheme}://file{}/caf%C3%A9%20%23%3F%20O%27Brien{suffix}?{}session=vscode-chat-session%3A%2F%2Flocal%2F{TOKEN}", self.root.display(), if cold { "" } else { "windowId=_blank&" });
        let final_call = calls.last().unwrap();
        assert!(
            final_call.iter().any(|arg| arg == "--open-url"),
            "{final_call:?}"
        );
        assert!(
            final_call.contains(&expected),
            "{final_call:?}; expected {expected}"
        );
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn printed_native_command_opens_a_cold_default_profile_after_a_real_readiness_probe() {
    let fixture = Fixture::new("cold", STORE, false);
    fixture.native(false);
    let target = fixture.metadata(false);
    let printed = success(fixture.run(&["--print", &format!("VSCode:{ID}")]));
    assert_eq!(
        printed,
        format!(
            "'{}' --open 'VSCode:{ID}'\n",
            env!("CARGO_BIN_EXE_session-finder").replace('\'', "'\\''")
        )
    );
    assert!(fixture.calls().is_empty());
    success(
        fixture
            .command("/bin/sh")
            .args(["-c", &printed])
            .output()
            .unwrap(),
    );
    fixture.assert_opened(&target, "vscode", true);
    fs::write(fixture.storage.join("workspace.json"), "{}").unwrap();
    assert!(success(fixture.run(&["--print", ID])).starts_with('#'));
    assert!(!fixture.run(&["--open", ID]).status.success());
    assert_eq!(
        fixture.calls().len(),
        4,
        "stale metadata must not launch the editor"
    );
}

#[test]
fn warm_custom_insiders_journal_opens_the_exact_workspace_file() {
    let fixture = Fixture::new(
        "warm",
        "custom/Code - Insiders/User/workspaceStorage/hash",
        true,
    );
    fixture.native(true);
    let target = fixture.metadata(true);
    fs::write(fixture.root.join("ready"), "").unwrap();
    success(fixture.run(&["--open", ID]));
    fixture.assert_opened(&target, "vscode-insiders", false);
}

#[test]
fn missing_metadata_is_refused_before_launch_and_editor_failure_is_reported() {
    let fixture = Fixture::new("failure", STORE, false);
    fixture.native(false);
    let output = fixture.run(&["--open", ID]);
    assert!(!output.status.success());
    assert!(fixture.calls().is_empty());
    assert!(success(fixture.run(&["--print", ID])).starts_with('#'));
    fixture.metadata(false);
    let output = fixture
        .command(env!("CARGO_BIN_EXE_session-finder"))
        .env("CODE_FAIL", "1")
        .args(["--open", ID])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains('7'));
    assert!(fixture
        .calls()
        .iter()
        .any(|call| call.iter().any(|arg| arg == "--new-window")));
    assert!(!fixture
        .calls()
        .iter()
        .any(|call| call.iter().any(|arg| arg == "--open-url")));
}

#[test]
fn unsupported_storage_and_hook_transcripts_keep_the_workspace_fallback() {
    for (label, store) in [
        ("hook", STORE),
        ("remote", ".vscode-server/data/User/workspaceStorage/hash"),
        ("nonstandard", "custom/hash"),
    ] {
        let fixture = Fixture::new(label, store, true);
        if label == "hook" {
            let transcripts = fixture.storage.join("GitHub.copilot-chat/transcripts");
            fs::create_dir_all(&transcripts).unwrap();
            fs::write(transcripts.join(format!("{ID}.jsonl")), format!("{{\"type\":\"session.start\",\"data\":{{\"sessionId\":\"{ID}\"}}}}\n{{\"type\":\"user.message\",\"data\":{{\"content\":\"Hook request\"}}}}\n")).unwrap();
        } else {
            fixture.native(false);
        }
        let target = fixture.metadata(false);
        let printed = success(fixture.run(&["--print", ID]));
        assert_eq!(
            printed,
            format!(
                "code '{}'\n",
                target.to_str().unwrap().replace('\'', "'\\''")
            )
        );
        assert!(!fixture.run(&["--open", ID]).status.success(), "{label}");
        assert!(fixture.calls().is_empty(), "{label}");
    }
}

#[test]
fn empty_window_native_history_has_no_exact_open_target() {
    let fixture = Fixture::new("empty-window", STORE, false);
    let chats = fixture
        .storage
        .ancestors()
        .nth(2)
        .unwrap()
        .join("globalStorage/emptyWindowChatSessions");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join(format!("{ID}.json")),
        serde_json::json!({"requests":[{"message":"Empty window request"}]}).to_string(),
    )
    .unwrap();
    assert!(success(fixture.run(&["--print", ID])).starts_with('#'));
    assert!(!fixture.run(&["--open", ID]).status.success());
    assert!(fixture.calls().is_empty());
}
