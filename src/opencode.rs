use crate::{BorrowedStr, Session, Source};
use rusqlite::{Connection, OpenFlags, Statement};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

pub fn database_path(home: &Path) -> io::Result<PathBuf> {
    resolve_database_path(
        home,
        std::env::var_os("SESSION_FINDER_OPENCODE_DB").map(PathBuf::from),
        std::env::var_os("OPENCODE_DB").map(PathBuf::from),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
    )
}

fn resolve_database_path(
    home: &Path,
    finder: Option<PathBuf>,
    opencode: Option<PathBuf>,
    data: Option<PathBuf>,
) -> io::Result<PathBuf> {
    let path = if let Some(path) = finder.filter(|path| !path.as_os_str().is_empty()) {
        path
    } else {
        let data = data
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| home.join(".local/share"))
            .join("opencode");
        match opencode.filter(|path| !path.as_os_str().is_empty()) {
            Some(path) if path == Path::new(":memory:") => path,
            Some(path) => data.join(path),
            None => data.join("opencode.db"),
        }
    };
    if path == Path::new(":memory:") {
        Ok(path)
    } else {
        std::path::absolute(path)
    }
}

pub(crate) fn is_session_id(id: &str) -> bool {
    id.strip_prefix("ses_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

// A transaction keeps messages and metadata in one read-only snapshot, including live WAL rows.
pub fn scan(database: &Path) -> io::Result<Vec<Session>> {
    if database == Path::new(":memory:") || !database.try_exists()? {
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
    scan_connection(&connection)
}

fn scan_connection(connection: &Connection) -> io::Result<Vec<Session>> {
    let mut tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .map_err(io::Error::other)?;
    let tables = tables
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(io::Error::other)?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .map_err(io::Error::other)?;
    let v2_table = if tables.contains("session_v2") {
        Some("session_v2")
    } else if tables.contains("session") && tables.contains("session_message") {
        Some("session")
    } else {
        None
    };
    if let Some(table) = v2_table {
        // Migration retains old v1 rows; only v2 remains authoritative after deletion or edits.
        scan_schema(connection, table, true, tables.contains("session_part"))
            .map_err(io::Error::other)
    } else if tables.contains("session") {
        scan_schema(connection, "session", false, true).map_err(io::Error::other)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported OpenCode database schema",
        ))
    }
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    role: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    text: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    summary: Option<&'a RawValue>,
    #[serde(default)]
    synthetic: bool,
    #[serde(default)]
    ignored: bool,
}

#[derive(Deserialize)]
struct TextPart<'a> {
    #[serde(borrow, rename = "type")]
    kind: BorrowedStr<'a>,
    #[serde(borrow)]
    text: BorrowedStr<'a>,
    #[serde(default)]
    synthetic: bool,
    #[serde(default)]
    ignored: bool,
}

#[derive(Deserialize)]
struct Revert<'a> {
    #[serde(borrow, rename = "messageID")]
    message_id: BorrowedStr<'a>,
    #[serde(borrow, rename = "partID")]
    part_id: Option<&'a RawValue>,
}

fn scan_schema(
    connection: &Connection,
    table: &str,
    v2: bool,
    has_parts: bool,
) -> rusqlite::Result<Vec<Session>> {
    let has_revert: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='revert')",
        [table],
        |row| row.get(0),
    )?;
    // Table names come only from the fixed schema choices above.
    let sql = [
        "SELECT id,directory,time_updated,time_archived,title,",
        if has_revert {
            "CAST(revert AS BLOB)"
        } else {
            "NULL"
        },
        " FROM ",
        table,
        " WHERE parent_id IS NULL",
        " ORDER BY id",
    ]
    .concat();
    let mut roots = connection.prepare(&sql)?;
    let mut messages = connection.prepare(if v2 {
        "SELECT id,type,CAST(data AS BLOB) FROM session_message
         WHERE session_id=?1 AND type IN ('user','assistant')
         AND (?2 IS NULL OR seq < (SELECT seq FROM session_message WHERE session_id=?1 AND id=?2))
         ORDER BY seq,id"
    } else {
        "SELECT id,NULL,CAST(data AS BLOB) FROM message AS m
         WHERE session_id=?1 AND ?2 IS NULL AND NOT EXISTS (
             SELECT 1 FROM part AS p WHERE p.session_id=m.session_id AND p.message_id=m.id
             AND CASE WHEN json_valid(p.data) THEN json_extract(p.data,'$.type') END='compaction'
         ) ORDER BY time_created,id"
    })?;
    let mut parts = if has_parts {
        Some(connection.prepare(if v2 {
            "SELECT CAST(data AS BLOB) FROM session_part
             WHERE session_id=?1 AND message_id=?2 AND type='text' ORDER BY ordinal"
        } else {
            "SELECT CAST(data AS BLOB) FROM part
             WHERE session_id=?1 AND message_id=?2 ORDER BY id"
        })?)
    } else {
        None
    };
    let rows = roots.query_map([], |row| {
        let id: String = row.get(0)?;
        let directory: String = row.get(1)?;
        if !is_session_id(&id)
            || !Path::new(&directory).is_absolute()
            || directory.chars().any(char::is_control)
        {
            return Ok(None);
        }
        let revert_data = row.get_ref(5)?;
        let revert = if let Some(data) = revert_data.as_bytes_or_null()? {
            // shortcut: omit legacy or malformed staged reverts until their partial-part semantics are supported.
            let Ok(revert) = serde_json::from_slice::<Revert<'_>>(data) else {
                return Ok(None);
            };
            if !v2 || revert.part_id.is_some() {
                return Ok(None);
            }
            Some(revert)
        } else {
            None
        };
        let mut session = Session {
            source: Source::OpenCode,
            id,
            cwd: directory.into(),
            updated: row.get::<_, i64>(2)? / 1000,
            archived: row.get::<_, Option<i64>>(3)?.is_some(),
            search_text: String::new(),
            assistant_text: String::new(),
            preview: String::new(),
            vscode: None,
        };
        read_messages(
            &mut session,
            &mut messages,
            parts.as_mut(),
            v2,
            revert.as_ref().map(|revert| &*revert.message_id),
        )?;
        if session.preview.is_empty() {
            return Ok(None);
        }
        if let Some(title) = row.get::<_, Option<String>>(4)? {
            append(&mut session.search_text, &title);
        }
        Ok(Some(session))
    })?;
    rows.filter_map(Result::transpose).collect()
}

fn read_messages(
    session: &mut Session,
    messages: &mut Statement<'_>,
    mut parts: Option<&mut Statement<'_>>,
    v2: bool,
    revert: Option<&str>,
) -> rusqlite::Result<()> {
    let mut rows = messages.query((&session.id, revert))?;
    while let Some(row) = rows.next()? {
        let data = row.get_ref(2)?;
        let Ok(bytes) = data.as_bytes() else { continue };
        let Ok(message) = serde_json::from_slice::<Message<'_>>(bytes) else {
            continue;
        };
        if message.synthetic || message.ignored {
            continue;
        }
        let kind = if v2 {
            row.get::<_, Option<String>>(1)?
        } else {
            None
        };
        let role = kind.as_deref().or(message.role.as_deref());
        let assistant = match role {
            Some("user") => false,
            Some("assistant") if !message.summary.is_some_and(|value| value.get() == "true") => {
                true
            }
            _ => continue,
        };
        if !v2 || assistant {
            if let Some(parts) = parts.as_deref_mut() {
                let id: String = row.get(0)?;
                let mut parts = parts.query((&session.id, id))?;
                while let Some(part) = parts.next()? {
                    if let Ok(bytes) = part.get_ref(0)?.as_bytes() {
                        append_part(session, bytes, assistant);
                    }
                }
            }
        }
        if v2 {
            if assistant {
                if let Some(content) = message
                    .content
                    .and_then(|content| serde_json::from_str::<Vec<&RawValue>>(content.get()).ok())
                {
                    for part in content {
                        append_part(session, part.get().as_bytes(), true);
                    }
                }
            } else if let Some(text) = message.text {
                append_message(session, &text, false);
            }
        }
    }
    Ok(())
}

fn append_part(session: &mut Session, data: &[u8], assistant: bool) {
    if let Ok(part) = serde_json::from_slice::<TextPart<'_>>(data) {
        if &*part.kind == "text" && !part.synthetic && !part.ignored {
            append_message(session, &part.text, assistant);
        }
    }
}

fn append_message(session: &mut Session, text: &str, assistant: bool) {
    if !assistant && session.preview.is_empty() && !text.trim().is_empty() {
        session.preview = text.chars().take(240).collect();
    }
    append(
        if assistant {
            &mut session.assistant_text
        } else {
            &mut session.search_text
        },
        text,
    );
}

fn append(target: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2_SCHEMA: &str = "
        CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
            title TEXT, time_updated INTEGER NOT NULL, time_archived INTEGER);
        CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL, data TEXT NOT NULL);
        CREATE TABLE session_part (message_id TEXT NOT NULL, session_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL,
            PRIMARY KEY(message_id, ordinal));";

    const V1_SCHEMA: &str = "
        CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
            title TEXT, time_updated INTEGER NOT NULL, time_archived INTEGER);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);";

    fn fixture(schema: &str) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(schema).unwrap();
        connection
    }

    fn user(connection: &Connection, session: &str, id: &str, seq: i64, data: &str) {
        connection
            .execute(
                "INSERT INTO session_message VALUES (?1,?2,'user',?3,0,0,?4)",
                (id, session, seq, data),
            )
            .unwrap();
    }

    #[test]
    fn v2_preserves_prompt_and_answer_order_and_metadata() {
        let connection = fixture(V2_SCHEMA);
        connection.execute_batch(
            "INSERT INTO session_v2 VALUES ('ses_main',NULL,'/work/O''Brien/repo','useful title',12345,0);
             INSERT INTO session_message VALUES ('msg_answer','ses_main','assistant',2,0,0,
                '{\"content\":[{\"type\":\"text\",\"text\":\"inline tail\"},{\"type\":\"reasoning\",\"text\":\"private reasoning\"}]}');
             INSERT INTO session_part VALUES ('msg_answer','ses_main',2,'text','{\"type\":\"text\",\"text\":\"second answer\"}');
             INSERT INTO session_part VALUES ('msg_answer','ses_main',1,'text','{\"type\":\"text\",\"text\":\"first answer\"}');
             INSERT INTO session_part VALUES ('msg_answer','ses_main',3,'tool','{\"type\":\"tool\",\"text\":\"tool output\"}');
             INSERT INTO session_part VALUES ('msg_answer','ses_main',4,'reasoning','{\"type\":\"reasoning\",\"text\":\"private reasoning\"}');
             INSERT INTO session_part VALUES ('msg_answer','ses_other',5,'text','{\"type\":\"text\",\"text\":\"wrong owner\"}');"
        ).unwrap();
        user(
            &connection,
            "ses_main",
            "msg_later",
            3,
            r#"{"text":"later prompt"}"#,
        );
        user(
            &connection,
            "ses_main",
            "msg_first",
            1,
            r#"{"text":"first \"prompt\" β","files":[{"data":"attachment bytes"}]}"#,
        );
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ses_main");
        assert_eq!(found[0].cwd, Path::new("/work/O'Brien/repo"));
        assert_eq!(found[0].updated, 12);
        assert!(found[0].archived);
        assert_eq!(found[0].preview, "first \"prompt\" β");
        assert_eq!(
            found[0].search_text,
            "first \"prompt\" β\nlater prompt\nuseful title"
        );
        assert_eq!(
            found[0].assistant_text,
            "first answer\nsecond answer\ninline tail"
        );
    }

    #[test]
    fn v2_excludes_subagents_synthetic_and_invalid_local_sessions() {
        let connection = fixture(V2_SCHEMA);
        for (id, parent, cwd) in [
            ("ses_main", None, "/work"),
            ("ses_child", Some("ses_main"), "/work"),
            ("ses_../unsafe", None, "/work"),
            ("ses_relative", None, "relative"),
            ("ses_empty", None, ""),
            ("ses_control", None, "/work\nother"),
            ("ses_machine", None, "/work"),
        ] {
            connection
                .execute(
                    "INSERT INTO session_v2 VALUES (?1,?2,?3,NULL,1000,NULL)",
                    (id, parent, cwd),
                )
                .unwrap();
            if id != "ses_machine" {
                user(&connection, id, id, 0, r#"{"text":"human prompt"}"#);
            }
        }
        for (id, kind, data) in [
            (
                "msg_synthetic",
                "synthetic",
                r#"{"text":"synthetic prompt"}"#,
            ),
            ("msg_system", "system", r#"{"text":"system instructions"}"#),
            (
                "msg_compaction",
                "compaction",
                r#"{"summary":"compaction summary"}"#,
            ),
            ("msg_shell", "shell", r#"{"output":"shell output"}"#),
            ("msg_bad", "user", "not-json"),
            ("msg_blank", "user", r#"{"text":" \n "}"#),
            (
                "msg_marked",
                "user",
                r#"{"text":"synthetic text","synthetic":true}"#,
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO session_message VALUES (?1,'ses_machine',?2,0,0,0,?3)",
                    (id, kind, data),
                )
                .unwrap();
        }
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ses_main");
    }

    #[test]
    fn v1_reads_human_text_and_excludes_synthetic_parts_and_summary_answers() {
        let connection = fixture(V1_SCHEMA);
        connection.execute_batch(
            "INSERT INTO session VALUES ('ses_legacy',NULL,'/legacy','legacy title',9000,NULL);
             INSERT INTO message VALUES ('msg_first','ses_legacy',1,0,'{\"role\":\"user\",\"summary\":{\"title\":\"ignored metadata\"}}');
             INSERT INTO message VALUES ('msg_later','ses_legacy',2,0,'{\"role\":\"user\"}');
             INSERT INTO message VALUES ('msg_answer','ses_legacy',3,0,'{\"role\":\"assistant\"}');
             INSERT INTO message VALUES ('msg_summary','ses_legacy',4,0,'{\"role\":\"assistant\",\"summary\":true}');
             INSERT INTO part VALUES ('prt_b','msg_first','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"second part\"}');
             INSERT INTO part VALUES ('prt_a','msg_first','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"first part\"}');
             INSERT INTO part VALUES ('prt_c','msg_first','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"synthetic\",\"synthetic\":true}');
             INSERT INTO part VALUES ('prt_d','msg_first','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"ignored\",\"ignored\":true}');
             INSERT INTO part VALUES ('prt_e','msg_first','ses_legacy',0,0,'{\"type\":\"file\",\"text\":\"file content\"}');
             INSERT INTO part VALUES ('prt_f','msg_later','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"later prompt\"}');
             INSERT INTO part VALUES ('prt_g','msg_answer','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"legacy answer\"}');
             INSERT INTO part VALUES ('prt_h','msg_answer','ses_legacy',0,0,'{\"type\":\"reasoning\",\"text\":\"reasoning\"}');
             INSERT INTO part VALUES ('prt_i','msg_summary','ses_legacy',0,0,'{\"type\":\"text\",\"text\":\"summary answer\"}');
             INSERT INTO part VALUES ('prt_j','msg_answer','ses_wrong',0,0,'{\"type\":\"text\",\"text\":\"wrong owner\"}');"
        ).unwrap();
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].search_text,
            "first part\nsecond part\nlater prompt\nlegacy title"
        );
        assert_eq!(found[0].assistant_text, "legacy answer");
        assert_eq!(found[0].preview, "first part");
        assert_eq!(found[0].updated, 9);
        assert!(!found[0].archived);
    }

    #[test]
    fn v2_schema_never_resurrects_legacy_sessions_after_deletion() {
        let connection = fixture(V2_SCHEMA);
        connection.execute_batch(V1_SCHEMA).unwrap();
        connection.execute_batch(
            "INSERT INTO session_v2 VALUES ('ses_migrated',NULL,'/current',NULL,1000,NULL);
             INSERT INTO session VALUES ('ses_migrated',NULL,'/old',NULL,1000,NULL);
             INSERT INTO session VALUES ('ses_unmigrated',NULL,'/legacy',NULL,1000,NULL);
             INSERT INTO message VALUES ('msg_stale','ses_migrated',0,0,'{\"role\":\"user\"}');
             INSERT INTO message VALUES ('msg_legacy','ses_unmigrated',0,0,'{\"role\":\"user\"}');
             INSERT INTO part VALUES ('prt_stale','msg_stale','ses_migrated',0,0,'{\"type\":\"text\",\"text\":\"stale prompt\"}');
             INSERT INTO part VALUES ('prt_legacy','msg_legacy','ses_unmigrated',0,0,'{\"type\":\"text\",\"text\":\"legacy prompt\"}');"
        ).unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
        user(
            &connection,
            "ses_migrated",
            "msg_current",
            0,
            r#"{"text":"current prompt"}"#,
        );
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].search_text, "current prompt");
        connection
            .execute("UPDATE session_message SET data='not-json'", [])
            .unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
        connection.execute("DELETE FROM session_v2", []).unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
    }

    #[test]
    fn staged_v2_revert_hides_boundary_and_later_messages_by_sequence() {
        let connection = fixture(V2_SCHEMA);
        connection
            .execute_batch(
                "ALTER TABLE session_v2 ADD COLUMN revert TEXT;
             INSERT INTO session_v2 VALUES ('ses_reverted',NULL,'/work',NULL,1000,NULL,
                 '{\"messageID\":\"msg_aaa_boundary\"}');
             INSERT INTO session_message VALUES ('msg_answer','ses_reverted','assistant',1,0,0,
                 '{\"content\":[{\"type\":\"text\",\"text\":\"visible answer\"}]}');
             INSERT INTO session_message VALUES ('msg_hidden','ses_reverted','assistant',3,0,0,
                 '{\"content\":[{\"type\":\"text\",\"text\":\"hidden answer\"}]}');",
            )
            .unwrap();
        user(
            &connection,
            "ses_reverted",
            "msg_zzz_visible",
            0,
            r#"{"text":"visible prompt"}"#,
        );
        user(
            &connection,
            "ses_reverted",
            "msg_aaa_boundary",
            2,
            r#"{"text":"hidden prompt"}"#,
        );
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].search_text, "visible prompt");
        assert_eq!(found[0].assistant_text, "visible answer");
        for revert in [
            "not-json",
            r#"{"messageID":"msg_missing"}"#,
            r#"{"messageID":"msg_aaa_boundary","partID":"prt_old"}"#,
        ] {
            connection
                .execute("UPDATE session_v2 SET revert=?1", [revert])
                .unwrap();
            assert!(scan_connection(&connection).unwrap().is_empty());
        }
    }

    #[test]
    fn legacy_partial_revert_is_conservatively_omitted() {
        let connection = fixture(V1_SCHEMA);
        connection
            .execute_batch(
                "ALTER TABLE session ADD COLUMN revert TEXT;
             INSERT INTO session VALUES ('ses_reverted',NULL,'/work',NULL,1000,NULL,
                 '{\"messageID\":\"msg_legacy\",\"partID\":\"prt_hidden\"}');
             INSERT INTO message VALUES ('msg_legacy','ses_reverted',0,0,'{\"role\":\"user\"}');
             INSERT INTO part VALUES ('prt_hidden','msg_legacy','ses_reverted',0,0,
                 '{\"type\":\"text\",\"text\":\"hidden prompt\"}');",
            )
            .unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
    }

    #[test]
    fn v1_compaction_requests_are_not_human_prompts() {
        let connection = fixture(V1_SCHEMA);
        connection.execute_batch(
            "INSERT INTO session VALUES ('ses_compaction',NULL,'/work',NULL,1000,NULL);
             INSERT INTO message VALUES ('msg_compaction','ses_compaction',0,0,'{\"role\":\"user\"}');
             INSERT INTO part VALUES ('prt_a','msg_compaction','ses_compaction',0,0,
                 '{\"type\":\"text\",\"text\":\"summarize the conversation\"}');
             INSERT INTO part VALUES ('prt_b','msg_compaction','ses_compaction',0,0,
                 '{\"type\":\"compaction\",\"auto\":true}');"
        ).unwrap();
        assert!(scan_connection(&connection).unwrap().is_empty());
    }

    #[test]
    fn pre_split_v2_database_can_use_session_table_and_inline_answers() {
        let connection = fixture(V2_SCHEMA);
        connection
            .execute_batch(
                "ALTER TABLE session_v2 RENAME TO session;
             DROP TABLE session_part;
             INSERT INTO session VALUES ('ses_next',NULL,'/next',NULL,1000,NULL);
             INSERT INTO session_message VALUES ('msg_answer','ses_next','assistant',1,0,0,
                 '{\"content\":[{\"type\":\"text\",\"text\":\"inline answer\"}]}');",
            )
            .unwrap();
        user(
            &connection,
            "ses_next",
            "msg_user",
            0,
            r#"{"text":"next prompt"}"#,
        );
        let found = scan_connection(&connection).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].assistant_text, "inline answer");
    }

    #[test]
    fn invalid_schema_and_sql_types_are_reported() {
        let unknown = Connection::open_in_memory().unwrap();
        assert!(scan_connection(&unknown).is_err());
        let connection = fixture(V2_SCHEMA);
        connection
            .execute_batch(
                "INSERT INTO session_v2 VALUES ('ses_bad',NULL,'/work',NULL,'bad timestamp',NULL)",
            )
            .unwrap();
        user(
            &connection,
            "ses_bad",
            "msg_user",
            0,
            r#"{"text":"prompt"}"#,
        );
        assert!(scan_connection(&connection).is_err());
    }

    #[test]
    fn readonly_scan_sees_wal_edits_and_deletion_without_creating_missing_database() {
        let root = std::env::temp_dir().join(format!(
            "opencode-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("opencode.db");
        assert!(scan(&database).unwrap().is_empty());
        assert!(!database.exists());
        assert!(scan(Path::new(":memory:")).unwrap().is_empty());
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        writer.execute_batch(V2_SCHEMA).unwrap();
        writer
            .execute_batch("INSERT INTO session_v2 VALUES ('ses_live',NULL,'/work',NULL,1000,NULL)")
            .unwrap();
        user(
            &writer,
            "ses_live",
            "msg_live",
            0,
            r#"{"text":"first prompt"}"#,
        );
        assert_eq!(scan(&database).unwrap()[0].search_text, "first prompt");
        writer
            .execute(
                "UPDATE session_message SET data=?1",
                [r#"{"text":"changed prompt"}"#],
            )
            .unwrap();
        assert_eq!(scan(&database).unwrap()[0].search_text, "changed prompt");
        writer.execute("DELETE FROM session_v2", []).unwrap();
        assert!(scan(&database).unwrap().is_empty());
        drop(writer);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_ids_cannot_escape_native_id_syntax() {
        for valid in ["ses_abc123", "ses_A-b_c"] {
            assert!(is_session_id(valid), "{valid:?}");
        }
        for invalid in [
            "",
            "ses_",
            "abc",
            "ses_../path",
            "ses_has space",
            "ses_\n",
            "ses_β",
            "ses_\u{1b}[31m",
        ] {
            assert!(!is_session_id(invalid), "{invalid:?}");
        }
    }

    #[test]
    fn database_path_matches_opencode_overrides_and_xdg_defaults() {
        let home = Path::new("/home/user");
        for (finder, opencode, data, expected) in [
            (
                None,
                None,
                None,
                "/home/user/.local/share/opencode/opencode.db",
            ),
            (None, None, Some("/data"), "/data/opencode/opencode.db"),
            (
                None,
                None,
                Some(""),
                "/home/user/.local/share/opencode/opencode.db",
            ),
            (
                None,
                Some("channel.db"),
                Some("/data"),
                "/data/opencode/channel.db",
            ),
            (
                None,
                Some("/custom/store.db"),
                Some("/data"),
                "/custom/store.db",
            ),
            (
                Some("/finder.db"),
                Some("/custom.db"),
                Some("/data"),
                "/finder.db",
            ),
            (None, Some(":memory:"), None, ":memory:"),
        ] {
            assert_eq!(
                resolve_database_path(
                    home,
                    finder.map(PathBuf::from),
                    opencode.map(PathBuf::from),
                    data.map(PathBuf::from)
                )
                .unwrap(),
                Path::new(expected)
            );
        }
    }

    #[test]
    fn relative_database_overrides_are_pinned_before_changing_directory() {
        let resolved = resolve_database_path(
            Path::new("/home/user"),
            Some("relative.db".into()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            resolved,
            std::env::current_dir().unwrap().join("relative.db")
        );
    }
}
