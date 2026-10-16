use super::{json_lines::JsonLines, BorrowedStr, Session, Source};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Deserialize)]
struct ClaudeRow<'a> {
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    cwd: Option<&'a RawValue>,
    #[serde(rename = "type", borrow)]
    kind: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct ClaudeMessage<'a> {
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct ClaudeTextBlock<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    text: Option<BorrowedStr<'a>>,
}

#[cfg(test)]
pub fn scan(root: &Path) -> Vec<Session> {
    let mut sessions: Vec<_> = session_files(root)
        .iter()
        .filter_map(|path| parse_file(path).unwrap())
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated));
    sessions
}

pub fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect(root, &mut files);
    files
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(&path, files);
        } else if kind.is_file()
            && path.extension().is_some_and(|ext| ext == "jsonl")
            && path
                .file_stem()
                .is_some_and(|stem| is_uuid(&stem.to_string_lossy()))
        {
            files.push(path);
        }
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

pub fn parse_file(path: &Path) -> io::Result<Option<Session>> {
    parse_reader(path, BufReader::new(fs::File::open(path)?))
}

fn parse_reader(path: &Path, reader: impl BufRead) -> io::Result<Option<Session>> {
    let Some(id) = path.file_stem() else {
        return Ok(None);
    };
    let id = id.to_string_lossy().into_owned();
    let mut cwd = None;
    let mut search_text = String::new();
    let mut assistant_text = String::new();
    let mut preview = String::new();
    let mut updated = 0;
    let mut genuine = false;
    let mut lines = JsonLines::new(reader);
    while let Some(line) = lines.next_line()? {
        let Ok(row) = serde_json::from_slice::<ClaudeRow<'_>>(line) else {
            continue;
        };
        if row.session_id.as_deref() != Some(id.as_str()) {
            continue;
        }
        if let Some(dir) = row
            .cwd
            .and_then(|raw| serde_json::from_str::<BorrowedStr<'_>>(raw.get()).ok())
        {
            cwd.get_or_insert_with(|| PathBuf::from(&*dir));
        }
        if let Some(ts) = row
            .timestamp
            .and_then(|raw| serde_json::from_str::<BorrowedStr<'_>>(raw.get()).ok())
            .and_then(|text| parse_timestamp(&text))
        {
            updated = updated.max(ts);
        }
        if !matches!(row.kind.as_deref(), Some("user" | "assistant")) {
            continue;
        }
        let assistant = row.kind.as_deref() == Some("assistant");
        genuine |= !assistant;
        let Some(message) = row.message else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<ClaudeMessage<'_>>(message.get()) else {
            continue;
        };
        if let Some(text) = user_text(message.content) {
            if !text.trim().is_empty() {
                if preview.is_empty() && !assistant {
                    preview = text.chars().take(240).collect();
                }
                let target = if assistant {
                    &mut assistant_text
                } else {
                    &mut search_text
                };
                if !target.is_empty() {
                    target.push('\n');
                }
                target.push_str(&text);
            }
        }
    }
    if !genuine {
        return Ok(None);
    }
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    if updated == 0 {
        updated = fs::metadata(path)?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs() as i64;
    }
    Ok(Some(Session {
        source: Source::Claude,
        id,
        cwd,
        updated,
        search_text,
        assistant_text,
        preview,
        archived: false,
    }))
}

fn user_text(value: Option<&RawValue>) -> Option<String> {
    let raw = value?.get();
    if let Ok(text) = serde_json::from_str::<BorrowedStr<'_>>(raw) {
        return Some(text.0.into_owned());
    }
    let parts = serde_json::from_str::<Vec<&RawValue>>(raw).ok()?;
    let mut text = String::new();
    let mut first = true;
    for part in parts {
        let Ok(part) = serde_json::from_str::<ClaudeTextBlock<'_>>(part.get()) else {
            continue;
        };
        if part.kind.as_deref() == Some("text") {
            if let Some(content) = part.text {
                if !first {
                    text.push(' ');
                }
                first = false;
                text.push_str(&content);
            }
        }
    }
    Some(text)
}

fn parse_timestamp(value: &str) -> Option<i64> {
    crate::timestamp::parse(value).or_else(|| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn read_errors_do_not_return_a_partial_session() {
        let prefix = b"{\"type\":\"user\",\"sessionId\":\"123e4567-e89b-12d3-a456-426614174000\",\"cwd\":\"/work\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"message\":{\"content\":\"partial conversation\"}}\n";
        let reader = crate::json_lines::reader_failing_after(prefix);
        let path = Path::new("123e4567-e89b-12d3-a456-426614174000.jsonl");
        assert_eq!(
            parse_reader(path, reader).unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }

    #[test]
    fn timestamps_handle_offsets_fractions_and_calendar_boundaries() {
        for (value, expected) in [
            ("1970-01-01T00:00:00Z", 0),
            ("2025-01-01t00:00:00Z", 1_735_689_600),
            ("2025-01-01T00:00:00z", 1_735_689_600),
            ("2025-01-01t00:00:00.123z", 1_735_689_600),
            ("1969-12-31T23:59:59Z", -1),
            ("1970-01-01T01:30:00.123456+01:30", 0),
            ("1969-12-31T22:30:00-01:30", 0),
            ("2000-02-29T00:00:00Z", 951_782_400),
            ("1900-03-01T00:00:00Z", -2_203_891_200),
            ("2016-12-31T23:59:60Z", 1_483_228_800),
            ("1735787045", 1_735_787_045),
        ] {
            assert_eq!(parse_timestamp(value), Some(expected), "{value}");
        }
    }

    #[test]
    fn rejects_malformed_timestamps() {
        for value in [
            "2025-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2025-04-31T00:00:00Z",
            "2025-01-01T24:00:00Z",
            "2025-01-01T00:60:00Z",
            "2025-01-01T00:00:61Z",
            "2025-01-01T-1:00:00Z",
            "2025-01-01X00:00:00Z",
            "2025-01-01T00-00:00Z",
            "2025-01-01T00:00:00.Z",
            "2025-01-01T00:00:00+24:00",
            "2025-01-01T00:00:00+00:60",
            "2025-01-01T00:00:00+01-30",
            "2025-01-01T00:00:00+01:30junk",
            "2025-01-01T00:00:00",
            "2025-01-01T00:00:00.123",
            "2025-01-01T00:00:00é",
        ] {
            assert_eq!(parse_timestamp(value), None, "{value}");
        }
    }

    #[test]
    fn scans_conversation_and_ignores_sidecar_and_bad_lines() {
        let root = std::env::temp_dir().join(format!(
            "claude-scan-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::create_dir_all(&root);
        let id = "123e4567-e89b-12d3-a456-426614174000";
        fs::write(root.join(format!("{id}.jsonl")), format!(
            "{{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"/work\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"find this phrase\"}}]}}}}\nnot-json\n{{\"type\":\"assistant\",\"sessionId\":\"{id}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"assistant answer\"}},{{\"type\":\"tool_use\",\"text\":\"exclude tool\"}}]}}}}\n"
        )).unwrap();
        fs::write(
            root.join("agent-123e4567-e89b-12d3-a456-426614174000.jsonl"),
            "{}",
        )
        .unwrap();
        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, id);
        assert_eq!(found[0].cwd, PathBuf::from("/work"));
        assert_eq!(found[0].search_text, "find this phrase");
        assert_eq!(
            serde_json::to_value(&found[0]).unwrap()["assistant_text"],
            "assistant answer"
        );
        assert_eq!(found[0].updated, 1_735_787_045);
    }

    #[test]
    fn reads_escaped_string_and_mixed_content_blocks() {
        let root = std::env::temp_dir().join(format!(
            "claude-content-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let id = "123e4567-e89b-12d3-a456-426614174000";
        let first = serde_json::json!({
            "type": "user",
            "sessionId": id,
            "cwd": "/tmp/O'Brien\\work",
            "timestamp": "2025-01-02T03:04:05.000Z",
            "message": {"content": "quote \" and slash \\"}
        });
        let first_json = first.to_string();
        let parsed: ClaudeRow<'_> = serde_json::from_str(&first_json).unwrap();
        assert!(matches!(
            parsed.kind,
            Some(BorrowedStr(Cow::Borrowed("user")))
        ));
        assert!(
            matches!(parsed.session_id, Some(BorrowedStr(Cow::Borrowed(value))) if value == id)
        );
        let second = serde_json::json!({
            "type": "user",
            "sessionId": id,
            "message": {"content": [
                {"type": "image", "text": "ignore"},
                7,
                {"type": "text", "text": "alpha"},
                {"type": "text", "text": ""},
                {"type": "text", "text": "β"}
            ]}
        });
        let malformed_cwd = serde_json::json!({
            "type": "user",
            "sessionId": id,
            "cwd": 7,
            "message": {"content": "keep despite bad cwd"}
        });
        fs::write(
            root.join(format!("{id}.jsonl")),
            format!("{first}\n{second}\n{malformed_cwd}\n"),
        )
        .unwrap();

        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cwd, PathBuf::from("/tmp/O'Brien\\work"));
        assert_eq!(
            found[0].search_text,
            "quote \" and slash \\\nalpha  β\nkeep despite bad cwd"
        );
        assert_eq!(found[0].preview, "quote \" and slash \\");
        fs::remove_dir_all(root).unwrap();
    }
}
