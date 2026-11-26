use rusqlite::Connection;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

const ID: &str = "ses_0123456789abcdefghijkl";

struct Fixture {
    root: PathBuf,
    db: PathBuf,
    cwd: PathBuf,
    writer: Connection,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "session-opencode-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = root.join("data ' store/opencode.db");
        let cwd = root.join("work O'Brien; echo bad");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        let writer = Connection::open(&db).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL, title TEXT, time_created INTEGER, time_updated INTEGER, time_archived INTEGER, revert TEXT);
            CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);
            CREATE TABLE session_part (message_id TEXT, session_id TEXT, ordinal INTEGER, type TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);").unwrap();
        writer.execute("INSERT INTO session_v2 VALUES (?1,NULL,?2,'Useful session title',1000,9000,NULL,NULL)", (ID, cwd.to_str().unwrap())).unwrap();
        for (id, kind, seq, data) in [
            ("msg_later", "user", 2, r#"{"text":"second human request"}"#),
            ("msg_first", "user", 1, r#"{"text":"find the café bug"}"#),
            (
                "msg_auto",
                "synthetic",
                3,
                r#"{"text":"hidden synthetic prompt"}"#,
            ),
            ("msg_assistant", "assistant", 4, r#"{"content":[]}"#),
        ] {
            writer
                .execute(
                    "INSERT INTO session_message VALUES (?1,?2,?3,?4,1000,2000,?5)",
                    (id, ID, kind, seq, data),
                )
                .unwrap();
        }
        for (ordinal, kind, data) in [
            (
                0,
                "reasoning",
                r#"{"type":"reasoning","text":"hidden reasoning"}"#,
            ),
            (
                1,
                "text",
                r#"{"type":"text","text":"answer with a \"quote\""}"#,
            ),
            (2, "tool", r#"{"type":"tool","text":"hidden tool output"}"#),
        ] {
            writer
                .execute(
                    "INSERT INTO session_part VALUES ('msg_assistant',?1,?2,?3,1000,2000,?4)",
                    (ID, ordinal, kind, data),
                )
                .unwrap();
        }
        let executable = root.join("bin/opencode");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$OPENCODE_DB\" \"$@\" > \"$RESUME_LOG\"\n",
        )
        .unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            root,
            db,
            cwd,
            writer,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", self.root.join("bin"))
            .env("SESSION_FINDER_CACHE", self.root.join("index.json"))
            .env("SESSION_FINDER_OPENCODE_DB", &self.db)
            .env("RESUME_LOG", self.root.join("resume-log"));
        for name in [
            "CLAUDE_ROOT",
            "CODEX_ROOT",
            "CODEX_ARCHIVE_ROOT",
            "COPILOT_ROOT",
            "VSCODE_ROOT",
            "CURSOR_DB",
        ] {
            command.env(format!("SESSION_FINDER_{name}"), self.root.join("missing"));
        }
        command
    }

    fn run(&self, args: &[&str]) -> String {
        success(self.command().args(args).output().unwrap())
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
fn opencode_search_resume_and_live_wal_updates_work_without_the_cli_installed() {
    let fixture = Fixture::new("history");
    let listed = fixture.run(&["--list"]);
    assert_eq!(
        listed,
        format!(
            "9\tOpenCode\t{ID}\t{}\tfind the café bug\n",
            fixture.cwd.display()
        )
    );
    let rows = fixture.run(&["--picker-rows"]);
    assert!(rows.starts_with(&format!("OpenCode:{ID}\t")));
    assert!(rows.contains(
        "find the café bug\nsecond human request"
            .replace('\n', " ")
            .as_str()
    ));
    assert!(rows.contains("Useful session title"));
    assert!(rows.contains("answer with a \"quote\""));
    assert!(!rows.contains("hidden"));

    let printed = fixture.run(&["--print", &format!("OpenCode:{ID}")]);
    assert!(printed.contains("opencode --standalone --session 'ses_0123456789abcdefghijkl'"));
    let environment = fixture.command();
    let mut shell = Command::new("/bin/sh");
    shell.env_clear();
    for (key, value) in environment.get_envs() {
        shell.env(key, value.unwrap());
    }
    success(shell.args(["-c", &printed]).output().unwrap());
    assert_eq!(
        fs::read_to_string(fixture.root.join("resume-log")).unwrap(),
        format!(
            "{}\n{}\n--standalone\n--session\n{ID}\n",
            fixture.cwd.display(),
            fixture.db.display()
        )
    );
    let opened = fixture.command().args(["--open", ID]).output().unwrap();
    assert!(!opened.status.success());
    assert!(String::from_utf8_lossy(&opened.stderr).contains("OpenCode"));

    fixture
        .writer
        .execute(
            "UPDATE session_message SET data=?1 WHERE id='msg_first'",
            [r#"{"text":"changed user request"}"#],
        )
        .unwrap();
    fixture
        .writer
        .execute(
            "UPDATE session_v2 SET time_updated=17000,time_archived=16000",
            [],
        )
        .unwrap();
    let updated = fixture.run(&["--picker-rows"]);
    assert!(updated.contains("changed user request"));
    assert!(!updated.contains("find the café bug"));
    assert!(updated.contains("OpenCode*"));
    assert!(fixture.run(&["--list"]).starts_with("17\tOpenCode*"));
    fixture
        .writer
        .execute_batch("DELETE FROM session_v2")
        .unwrap();
    assert!(fixture.run(&["--list"]).is_empty());
}

#[test]
fn opencode_database_path_overrides_and_xdg_defaults_select_the_same_store() {
    let fixture = Fixture::new("paths");
    for mode in ["default", "xdg", "relative", "absolute", "override"] {
        let data = if mode == "default" {
            fixture.root.join(".local/share")
        } else {
            fixture.root.join("xdg")
        };
        let db = if mode == "absolute" || mode == "override" {
            fixture.db.clone()
        } else {
            data.join("opencode").join(if mode == "relative" {
                "channel.db"
            } else {
                "opencode.db"
            })
        };
        if db != fixture.db {
            fs::create_dir_all(db.parent().unwrap()).unwrap();
            fixture
                .writer
                .execute("VACUUM INTO ?1", [db.to_str().unwrap()])
                .unwrap();
        }
        let mut command = fixture.command();
        command
            .env_remove("SESSION_FINDER_OPENCODE_DB")
            .env("XDG_DATA_HOME", &data);
        match mode {
            "default" => {
                command.env_remove("XDG_DATA_HOME");
            }
            "relative" => {
                command.env("OPENCODE_DB", "channel.db");
            }
            "absolute" => {
                command.env("OPENCODE_DB", &db);
            }
            "override" => {
                command
                    .env("OPENCODE_DB", "wrong.db")
                    .env("SESSION_FINDER_OPENCODE_DB", &db);
            }
            _ => (),
        }
        let output = success(command.args(["--print", ID]).output().unwrap());
        assert!(
            output.contains(&db.to_string_lossy().replace('\'', "'\\''")),
            "{mode}: {output}"
        );
    }
}

#[test]
fn corrupt_opencode_database_does_not_hide_other_providers_or_create_missing_files() {
    let fixture = Fixture::new("errors");
    let claude = fixture.root.join("claude");
    fs::create_dir_all(&claude).unwrap();
    fs::write(claude.join("123e4567-e89b-12d3-a456-426614174000.jsonl"), r#"{"type":"user","sessionId":"123e4567-e89b-12d3-a456-426614174000","cwd":"/work","message":{"content":"Claude request"}}"#).unwrap();
    let corrupt = fixture.root.join("corrupt.db");
    fs::write(&corrupt, "not sqlite").unwrap();
    let output = fixture
        .command()
        .env("SESSION_FINDER_CLAUDE_ROOT", &claude)
        .env("SESSION_FINDER_OPENCODE_DB", &corrupt)
        .args(["--list"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("Could not read OpenCode history"));
    assert!(success(output).contains("Claude request"));
    let missing = fixture.root.join("missing.db");
    assert!(fixture
        .command()
        .env("SESSION_FINDER_OPENCODE_DB", &missing)
        .args(["--list"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!missing.exists());
}
