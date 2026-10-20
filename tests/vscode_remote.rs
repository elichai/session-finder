use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

const ID: &str = "11111111-2222-4333-8444-555555555555";
const TOKEN: &str = "MTExMTExMTEtMjIyMi00MzMzLTg0NDQtNTU1NTU1NTU1NTU1";

struct Fixture {
    root: PathBuf,
    storage: PathBuf,
    host: String,
}

impl Fixture {
    fn new(label: &str, encoded_folder: &str, cwd: Option<&str>, mentioned: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "session-remote-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let host = fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_else(|_| "sf-fixture-host".to_owned())
            .trim()
            .to_owned();
        // Use the OS digest tool so fixture hashes do not depend on the reader's MD5.
        let mut digest = if cfg!(target_os = "macos") {
            let mut command = Command::new("/sbin/md5");
            command.arg("-q");
            command
        } else {
            Command::new("md5sum")
        }
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
        digest
            .stdin
            .take()
            .unwrap()
            .write_all(format!("vscode-remote://ssh-remote%2B{host}{encoded_folder}").as_bytes())
            .unwrap();
        let output = digest.wait_with_output().unwrap();
        assert!(output.status.success());
        let hash = String::from_utf8(output.stdout).unwrap();
        let storage = root
            .join(".vscode-server/data/User/workspaceStorage")
            .join(hash.split_whitespace().next().unwrap());
        let transcripts = storage.join("GitHub.copilot-chat/transcripts");
        fs::create_dir_all(&transcripts).unwrap();
        let events = [
            serde_json::json!({"type":"session.start","data":{"sessionId":ID,"context":{"cwd":cwd}}}),
            serde_json::json!({"type":"user.message","data":{"content":"Controlled remote request"}}),
            serde_json::json!({"type":"tool.execution_complete","data":{"arguments":serde_json::json!({"path":mentioned}).to_string()}}),
        ];
        let text = events
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(transcripts.join(format!("{ID}.jsonl")), text).unwrap();
        let browser = root.join("client browser");
        fs::write(
            &browser,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$BROWSER_LOG\"\nexit \"${BROWSER_EXIT:-0}\"\n",
        )
        .unwrap();
        fs::set_permissions(browser, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            root,
            storage,
            host,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("HOSTNAME", &self.host)
            .env("PATH", self.root.join("missing-bin"))
            .env("BROWSER", self.root.join("client browser"))
            .env("BROWSER_LOG", self.root.join("browser-url"))
            .env("SESSION_FINDER_VSCODE_ROOT", self.storage.parent().unwrap())
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
        command
    }

    fn url(&self, encoded_folder: &str) -> String {
        format!("vscode://vscode-remote/ssh-remote+{}{encoded_folder}?windowId=_blank&session=vscode-chat-session%3A%2F%2Flocal%2F{TOKEN}\n", self.host)
    }

    fn open(&self, encoded_folder: &str) {
        success(self.command().args(["--open", ID]).output().unwrap());
        assert_eq!(
            fs::read_to_string(self.root.join("browser-url")).unwrap(),
            self.url(encoded_folder)
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
fn remote_open_requests_a_new_window_and_copied_command_uses_the_same_route() {
    let fixture = Fixture::new("window", "/work/app", Some("/work/app"), None);
    fixture.open("/work/app");
    let printed = success(fixture.command().args(["--print", ID]).output().unwrap());
    assert!(printed.ends_with(&format!(" --open 'VSCode:{ID}'\n")));
    let environment = fixture.command();
    let mut command = Command::new("/bin/sh");
    command.env_clear();
    for (key, value) in environment.get_envs() {
        command.env(key, value.unwrap());
    }
    command.args(["-c", &printed]);
    success(command.output().unwrap());
    assert_eq!(
        fs::read_to_string(fixture.root.join("browser-url")).unwrap(),
        fixture.url("/work/app")
    );
}

#[test]
fn transcript_recovery_decodes_nested_paths_with_spaces_quotes_and_backslashes() {
    for (label, folder, encoded) in [
        ("spaces", "/work/my app", "/work/my%20app"),
        ("quotes", "/work/app \"two\"", "/work/app%20%22two%22"),
        ("backslashes", "/work/app\\two", "/work/app%5Ctwo"),
    ] {
        let fixture = Fixture::new(
            label,
            encoded,
            None,
            Some(&[folder, "/src/main.rs"].concat()),
        );
        fixture.open(encoded);
    }
}

#[test]
fn transcript_recovery_checks_the_quoted_folder_itself() {
    let fixture = Fixture::new("folder", "/work/app", None, Some("/work/app"));
    fixture.open("/work/app");
}

#[test]
fn root_workspace_is_recovered_from_cwd_and_transcript_paths() {
    let cwd = Fixture::new("root-cwd", "/", Some("/"), None);
    cwd.open("/");
    let path = Fixture::new("root-path", "/", None, Some("/src/main.rs"));
    path.open("/");
}

#[test]
fn workspace_prompt_preserves_escaped_characters_in_the_folder() {
    let fixture = Fixture::new(
        "prompt",
        "/work/caf%C3%A9%20%22tools%22%5Carchive",
        None,
        None,
    );
    let log = fixture
        .storage
        .join("GitHub.copilot-chat/debug-logs")
        .join(ID);
    fs::create_dir_all(&log).unwrap();
    fs::write(log.join("main.jsonl"), serde_json::json!({"prompt":"workspace with the following folders:\n- /work/café \"tools\"\\archive\n"}).to_string()).unwrap();
    fixture.open("/work/caf%C3%A9%20%22tools%22%5Carchive");
}

#[test]
fn double_escaped_prompt_distinguishes_folder_backslashes_from_newlines() {
    let fixture = Fixture::new("double-prompt", "/work/app%5Cnew", None, None);
    let log = fixture
        .storage
        .join("GitHub.copilot-chat/debug-logs")
        .join(ID);
    fs::create_dir_all(&log).unwrap();
    fs::write(
        log.join("main.jsonl"),
        r#"{"prompt":"workspace with the following folders:\\n- /work/app\\\\new\\n"}"#,
    )
    .unwrap();
    fixture.open("/work/app%5Cnew");
}

#[test]
fn a_different_folder_cannot_pass_the_storage_hash_check() {
    let fixture = Fixture::new(
        "mismatch",
        "/work/expected",
        Some("/work/wrong"),
        Some("/work/other/file.rs"),
    );
    let output = fixture.command().args(["--open", ID]).output().unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.join("browser-url").exists());
}

#[test]
fn browser_errors_are_reported_and_missing_browser_prints_a_client_url() {
    let fixture = Fixture::new("browser", "/work/app", Some("/work/app"), None);
    let output = fixture
        .command()
        .env("BROWSER_EXIT", "7")
        .args(["--open", ID])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains('7'));
    let output = fixture
        .command()
        .env_remove("BROWSER")
        .args(["--open", ID])
        .output()
        .unwrap();
    assert!(success(output).contains(&fixture.url("/work/app")));
}
