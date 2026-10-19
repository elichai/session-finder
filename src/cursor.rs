use crate::codex::is_uuid;
use crate::{BorrowedStr, Session, Source};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Composer<'a> {
    #[serde(default)]
    is_ephemeral: bool,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    is_best_of_n_subcomposer: bool,
    #[serde(borrow)]
    subagent_info: Option<&'a RawValue>,
    #[serde(borrow)]
    name: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    workspace_identifier: Option<Environment<'a>>,
    #[serde(borrow)]
    agent_location: Option<Location<'a>>,
    #[serde(borrow)]
    full_conversation_headers_only: Option<&'a RawValue>,
    #[serde(borrow)]
    conversation: Option<&'a RawValue>,
    created_at: Option<i64>,
    last_updated_at: Option<i64>,
    #[serde(default)]
    is_archived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Location<'a> {
    #[serde(borrow)]
    worktree_path: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    environment: Option<Environment<'a>>,
}

#[derive(Deserialize)]
struct Environment<'a> {
    #[serde(borrow)]
    uri: Option<Uri<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Uri<'a> {
    #[serde(borrow)]
    fs_path: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    scheme: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    path: Option<BorrowedStr<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bubble<'a> {
    #[serde(rename = "type")]
    kind: Option<u8>,
    #[serde(borrow)]
    bubble_id: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    text: Option<BorrowedStr<'a>>,
}

impl Composer<'_> {
    fn internal(&self) -> bool {
        self.is_ephemeral
            || self.is_draft
            || self.is_best_of_n_subcomposer
            || self.subagent_info.is_some()
    }

    fn cwd(&self) -> Option<PathBuf> {
        let location = self.agent_location.as_ref();
        location
            .and_then(|location| location.worktree_path.as_deref())
            .and_then(absolute_path)
            .or_else(|| {
                location
                    .and_then(|location| location.environment.as_ref())
                    .and_then(Environment::cwd)
            })
            .or_else(|| {
                self.workspace_identifier
                    .as_ref()
                    .and_then(Environment::cwd)
            })
    }
}

impl Environment<'_> {
    fn cwd(&self) -> Option<PathBuf> {
        let uri = self.uri.as_ref()?;
        if uri.scheme.as_deref().is_some_and(|scheme| scheme != "file") {
            return None;
        }
        uri.fs_path
            .as_deref()
            .or(uri.path.as_deref())
            .and_then(absolute_path)
    }
}

fn absolute_path(path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

// Read a consistent snapshot, including live WAL changes, without modifying Cursor history.
pub fn scan(database: &Path) -> io::Result<Vec<Session>> {
    if !database.try_exists()? {
        return Ok(Vec::new());
    }
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(io::Error::other)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(200))
        .map_err(io::Error::other)?;
    connection
        .execute_batch("BEGIN")
        .map_err(io::Error::other)?;
    scan_connection(&connection).map_err(io::Error::other)
}

fn scan_connection(connection: &Connection) -> rusqlite::Result<Vec<Session>> {
    let has_headers = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='composerHeaders')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let sql = if has_headers {
        "SELECT substr(d.key,14), CAST(d.value AS BLOB), CAST(h.value AS BLOB), h.lastUpdatedAt, h.isArchived
         FROM cursorDiskKV d LEFT JOIN composerHeaders h ON d.key='composerData:'||h.composerId
         WHERE d.key GLOB 'composerData:*' AND h.isSubagent IS NOT 1"
    } else {
        "SELECT substr(key,14), CAST(value AS BLOB), NULL, NULL, NULL
         FROM cursorDiskKV WHERE key GLOB 'composerData:*'"
    };
    let mut composers = connection.prepare(sql)?;
    let mut bubbles =
        connection.prepare("SELECT CAST(value AS BLOB) FROM cursorDiskKV WHERE key=?1")?;
    let rows = composers.query_map([], |row| {
        let id: String = row.get(0)?;
        if !is_uuid(&id) {
            return Ok(None);
        }
        let data = row.get_ref(1)?;
        let Ok(bytes) = data.as_bytes() else {
            return Ok(None);
        };
        let Ok(composer) = serde_json::from_slice::<Composer<'_>>(bytes) else {
            return Ok(None);
        };
        let header_data = row.get_ref(2)?;
        let header = header_data
            .as_bytes_or_null()?
            .and_then(|bytes| serde_json::from_slice::<Composer<'_>>(bytes).ok());
        if composer.internal() || header.as_ref().is_some_and(Composer::internal) {
            return Ok(None);
        }
        let Some(cwd) = header
            .as_ref()
            .and_then(Composer::cwd)
            .or_else(|| composer.cwd())
        else {
            return Ok(None);
        };
        let mut session = Session {
            source: Source::Cursor,
            id,
            cwd,
            updated: row
                .get::<_, Option<i64>>(3)?
                .or_else(|| header.as_ref().and_then(|header| header.last_updated_at))
                .or(composer.last_updated_at)
                .or(composer.created_at)
                .unwrap_or_default()
                / 1000,
            search_text: String::new(),
            assistant_text: String::new(),
            preview: String::new(),
            vscode: None,
            archived: row
                .get::<_, Option<bool>>(4)?
                .unwrap_or(composer.is_archived),
        };
        if let Some(parts) = composer
            .full_conversation_headers_only
            .or(composer.conversation)
            .and_then(|raw| serde_json::from_str::<Vec<&RawValue>>(raw.get()).ok())
        {
            for part in parts {
                let Ok(bubble) = serde_json::from_str::<Bubble<'_>>(part.get()) else {
                    continue;
                };
                if !matches!(bubble.kind, Some(1 | 2)) {
                    continue;
                }
                if let Some(text) = bubble.text {
                    append_text(&mut session, &text, bubble.kind == Some(2));
                } else if let Some(bubble_id) = bubble.bubble_id {
                    let key = ["bubbleId:", &session.id, ":", &bubble_id].concat();
                    bubbles
                        .query_row([key], |row| {
                            if let Some(bytes) = row.get_ref(0)?.as_bytes_or_null()? {
                                if let Ok(bubble) = serde_json::from_slice::<Bubble<'_>>(bytes) {
                                    if matches!(bubble.kind, Some(1 | 2)) {
                                        if let Some(text) = bubble.text {
                                            append_text(
                                                &mut session,
                                                &text,
                                                bubble.kind == Some(2),
                                            );
                                        }
                                    }
                                }
                            }
                            Ok(())
                        })
                        .optional()?;
                }
            }
        }
        if session.preview.is_empty() {
            return Ok(None);
        }
        if let Some(name) = header
            .as_ref()
            .and_then(|header| header.name.as_deref())
            .or(composer.name.as_deref())
        {
            session.search_text.push('\n');
            session.search_text.push_str(name);
        }
        Ok(Some(session))
    })?;
    rows.filter_map(Result::transpose).collect()
}

fn append_text(session: &mut Session, text: &str, assistant: bool) {
    if text.trim().is_empty() {
        return;
    }
    if session.preview.is_empty() && !assistant {
        session.preview = text.chars().take(240).collect();
    }
    let target = if assistant {
        &mut session.assistant_text
    } else {
        &mut session.search_text
    };
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);
             CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, value TEXT, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER);"
        ).unwrap();
        connection
    }

    #[test]
    fn indexes_user_bubbles_in_order_with_precise_worktree_and_archive_metadata() {
        let connection = fixture();
        let id = "123e4567-e89b-12d3-a456-426614174000";
        let data = serde_json::json!({
            "createdAt": 1000,
            "workspaceIdentifier": {"uri": {"fsPath": "/original", "scheme": "file"}},
            "fullConversationHeadersOnly": [
                7, {"type": 2, "bubbleId": "assistant"},
                {"type": 1, "bubbleId": "first"},
                {"type": 1, "bubbleId": "broken"},
                {"type": 1, "bubbleId": "missing"},
                {"type": 1, "bubbleId": "null"},
                {"type": 1, "bubbleId": "second"}
            ]
        });
        connection
            .execute(
                "INSERT INTO cursorDiskKV VALUES (?1,?2)",
                (format!("composerData:{id}"), data.to_string()),
            )
            .unwrap();
        connection.execute("INSERT INTO composerHeaders VALUES (?1,?2,9000,1,0)",
            (id, r#"{"name":"useful title","agentLocation":{"worktreePath":"/work/O'Brien\\repo","environment":{"uri":{"fsPath":"/wrong"}}}}"#)).unwrap();
        for (bubble_id, data) in [
            ("first", r#"{"type":1,"text":"first phrase"}"#),
            ("assistant", r#"{"type":2,"text":"do not index assistant"}"#),
            ("broken", "not-json"),
            ("second", r#"{"type":1,"text":"escaped \" quote and β"}"#),
        ] {
            connection
                .execute(
                    "INSERT INTO cursorDiskKV VALUES (?1,?2)",
                    (format!("bubbleId:{id}:{bubble_id}"), data),
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO cursorDiskKV VALUES (?1,NULL)",
                [format!("bubbleId:{id}:null")],
            )
            .unwrap();
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cwd, PathBuf::from("/work/O'Brien\\repo"));
        assert_eq!(found[0].updated, 9);
        assert!(found[0].archived);
        assert_eq!(found[0].preview, "first phrase");
        assert_eq!(
            serde_json::to_value(&found[0]).unwrap()["assistant_text"],
            "do not index assistant"
        );
        assert_eq!(
            found[0].search_text,
            "first phrase\nescaped \" quote and β\nuseful title"
        );
        connection
            .execute("UPDATE composerHeaders SET isSubagent=1", [])
            .unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
        connection
            .execute(
                "UPDATE composerHeaders SET isSubagent=0,value=?1",
                [r#"{"isEphemeral":true}"#],
            )
            .unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
    }

    #[test]
    fn collecting_sessions_preserves_sql_errors() {
        let connection = fixture();
        let id = "123e4567-e89b-12d3-a456-426614174000";
        connection
            .execute(
                "INSERT INTO cursorDiskKV VALUES (?1,?2)",
                (
                    format!("composerData:{id}"),
                    r#"{"workspaceIdentifier":{"uri":{"fsPath":"/work"}}}"#,
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO composerHeaders VALUES (?1,NULL,'invalid',0,0)",
                [id],
            )
            .unwrap();
        assert!(matches!(
            scan_connection(&connection),
            Err(rusqlite::Error::InvalidColumnType(3, _, _))
        ));
    }

    #[test]
    fn legacy_inline_messages_require_a_real_local_workspace() {
        let connection = fixture();
        connection
            .execute_batch("DROP TABLE composerHeaders")
            .unwrap();
        let id = "123e4567-e89b-12d3-a456-426614174000";
        let data = r#"{"createdAt":12345,"workspaceIdentifier":{"uri":{"scheme":"file","path":"/legacy"}},"conversation":[{"type":1,"text":"legacy user"},{"type":2,"text":"assistant"}]}"#;
        connection.execute(
            "INSERT INTO cursorDiskKV VALUES ('composerData:223e4567-e89b-12d3-a456-426614174000',NULL)",
            [],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO cursorDiskKV VALUES (?1,?2)",
                (format!("composerData:{id}"), data),
            )
            .unwrap();
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cwd, PathBuf::from("/legacy"));
        assert_eq!(found[0].search_text, "legacy user");
        assert_eq!(
            serde_json::to_value(&found[0]).unwrap()["assistant_text"],
            "assistant"
        );
        assert_eq!(found[0].updated, 12);
        connection
            .execute(
                "UPDATE cursorDiskKV SET value=?1",
                [data.replace("\"scheme\":\"file\"", "\"scheme\":\"vscode-remote\"")],
            )
            .unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
    }

    #[test]
    fn readonly_scan_sees_committed_wal_changes_and_missing_database_is_empty() {
        let root = std::env::temp_dir().join(format!(
            "cursor-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("state.vscdb");
        assert!(scan(&database).unwrap().is_empty());
        assert!(!database.exists());
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);",
            )
            .unwrap();
        let id = "123e4567-e89b-12d3-a456-426614174000";
        writer.execute("INSERT INTO cursorDiskKV VALUES (?1,?2)", (
            format!("composerData:{id}"),
            r#"{"workspaceIdentifier":{"uri":{"fsPath":"/work"}},"conversation":[{"type":1,"text":"first"}]}"#
        )).unwrap();
        assert_eq!(scan(&database).unwrap()[0].search_text, "first");
        writer.execute("UPDATE cursorDiskKV SET value=?1", [
            r#"{"workspaceIdentifier":{"uri":{"fsPath":"/work"}},"conversation":[{"type":1,"text":"changed"}]}"#
        ]).unwrap();
        assert_eq!(scan(&database).unwrap()[0].search_text, "changed");
        drop(writer);
        std::fs::remove_dir_all(root).unwrap();
    }
}
