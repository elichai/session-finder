use crate::{
    json_lines::JsonLines, timestamp::parse as parse_timestamp, BorrowedStr, Session, Source,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Deserialize)]
struct CodexRecord<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct SessionMeta<'a> {
    #[serde(borrow)]
    id: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    cwd: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    thread_source: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct ResponseItem<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    role: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct ContentBlock<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    text: Option<BorrowedStr<'a>>,
}

#[cfg(test)]
pub fn scan(root: &Path) -> Vec<Session> {
    let mut sessions: Vec<_> = session_files(root)
        .into_iter()
        .filter_map(|path| parse_file(&path).unwrap())
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated));
    sessions
}

pub fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rollouts(root, &mut files);
    files
}

fn collect_rollouts(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_rollouts(&path, files);
        } else if kind.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            files.push(path);
        }
    }
}

pub fn parse_file(path: &Path) -> io::Result<Option<Session>> {
    let modified = fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs() as i64;
    parse_reader(BufReader::new(fs::File::open(path)?), modified)
}

fn parse_reader(reader: impl BufRead, modified: i64) -> io::Result<Option<Session>> {
    let mut id = None;
    let mut cwd = None;
    let mut updated = None;
    let mut search_text = String::new();
    let mut assistant_text = String::new();
    let mut preview = String::new();

    let mut lines = JsonLines::new(reader);
    while let Some(line) = lines.next_line()? {
        let Ok(record) = serde_json::from_slice::<CodexRecord<'_>>(line) else {
            continue;
        };
        if let Some(raw) = record.timestamp {
            if let Some(timestamp) = serde_json::from_str::<BorrowedStr<'_>>(raw.get())
                .ok()
                .and_then(|text| parse_timestamp(&text))
            {
                updated = Some(updated.map_or(timestamp, |latest: i64| latest.max(timestamp)));
            }
        }
        if record.kind.as_deref() == Some("session_meta") {
            let Some(payload) = record.payload else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<SessionMeta<'_>>(payload.get()) else {
                continue;
            };
            let thread_source = meta
                .thread_source
                .and_then(|raw| serde_json::from_str::<BorrowedStr<'_>>(raw.get()).ok());
            if matches!(
                thread_source.as_deref(),
                Some("guardian_review" | "subagent")
            ) {
                return Ok(None);
            }
            let Some(meta_id) = meta.id else {
                continue;
            };
            let Some(meta_cwd) = meta.cwd else {
                continue;
            };
            if !is_uuid(&meta_id) || meta_cwd.is_empty() {
                continue;
            }
            id = Some(meta_id.0.into_owned());
            cwd = Some(PathBuf::from(meta_cwd.0.into_owned()));
        } else if record.kind.as_deref() == Some("response_item") {
            let Some(payload) = record.payload else {
                continue;
            };
            let Ok(payload) = serde_json::from_str::<ResponseItem<'_>>(payload.get()) else {
                continue;
            };
            if payload.kind.as_deref() != Some("message")
                || !matches!(payload.role.as_deref(), Some("user" | "assistant"))
            {
                continue;
            }
            if let Some(blocks) = payload
                .content
                .and_then(|raw| serde_json::from_str::<Vec<&RawValue>>(raw.get()).ok())
            {
                for raw in blocks {
                    let Ok(block) = serde_json::from_str::<ContentBlock<'_>>(raw.get()) else {
                        continue;
                    };
                    let assistant = payload.role.as_deref() == Some("assistant");
                    if block.kind.as_deref()
                        == Some(if assistant {
                            "output_text"
                        } else {
                            "input_text"
                        })
                    {
                        if let Some(text) = block.text {
                            let candidate = text.trim_start();
                            // Codex records injected context as user-role messages.
                            if !assistant
                                && (candidate.starts_with("# AGENTS.md instructions for ")
                                    || candidate.starts_with("<environment_context>")
                                    || candidate.starts_with("<external_codex_apps_"))
                            {
                                continue;
                            }
                            if !assistant && preview.is_empty() && !candidate.is_empty() {
                                preview = candidate.lines().next().unwrap_or_default().to_owned();
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
            }
        }
    }

    let (Some(id), Some(cwd)) = (id, cwd) else {
        return Ok(None);
    };
    Ok(Some(Session {
        source: Source::Codex,
        id,
        cwd,
        updated: updated.unwrap_or(modified),
        preview: if preview.is_empty() {
            search_text.lines().next().unwrap_or_default().to_owned()
        } else {
            preview
        },
        search_text,
        assistant_text,
        archived: false,
    }))
}

pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::io::Write;

    #[test]
    fn read_errors_do_not_return_a_partial_session() {
        let prefix = b"{\"type\":\"session_meta\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"payload\":{\"id\":\"123e4567-e89b-12d3-a456-426614174000\",\"cwd\":\"/work\"}}\n";
        let reader = crate::json_lines::reader_failing_after(prefix);
        assert_eq!(
            parse_reader(reader, 0).unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }

    #[test]
    fn rejects_malformed_timestamps() {
        for value in [
            "2025-01-01T00:00:00Zjunk",
            "2025-01-01T00-00:00Z",
            "2025-01-01T-1:00:00Z",
            "2025-02-29T00:00:00Z",
            "2025-01-01T00:00:00.Z",
            "1735787045",
        ] {
            assert_eq!(parse_timestamp(value), None, "{value}");
        }
    }

    #[test]
    fn scans_session_metadata_and_user_text() {
        let root = std::env::temp_dir().join(format!("codex-scan-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("rollout-fixture.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"session_meta","timestamp":"2026-01-02T03:04:05.000Z","payload":{{"id":"019c675d-0e41-7f71-925f-1ccc0da0f692","cwd":"/tmp/project"}}}}"#).unwrap();
        writeln!(file, r##"{{"type":"response_item","timestamp":"2026-01-02T03:04:05.000Z","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"# AGENTS.md instructions for /tmp/project"}}]}}}}"##).unwrap();
        writeln!(file, r#"{{"type":"response_item","timestamp":"2026-01-02T03:04:05.000Z","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<environment_context>local</environment_context>"}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"response_item","timestamp":"2026-01-02T03:04:05.000Z","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<external_codex_apps_open_page>internal</external_codex_apps_open_page>"}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"response_item","timestamp":"2026-01-02T03:04:05.000Z","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<wallet>real request</wallet>"}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"response_item","timestamp":"2026-01-02T03:04:06.000Z","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"fixture query"}}]}}}}"#).unwrap();
        let mixed = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "user", "content": [
                7,
                {"type": "image", "text": "ignore"},
                {"type": "input_text", "text": "quote \" and slash \\"}
            ]}
        });
        let mixed_json = mixed.to_string();
        let record: CodexRecord<'_> = serde_json::from_str(&mixed_json).unwrap();
        assert!(matches!(
            record.kind,
            Some(BorrowedStr(Cow::Borrowed("response_item")))
        ));
        let payload: ResponseItem<'_> =
            serde_json::from_str(record.payload.unwrap().get()).unwrap();
        assert!(matches!(
            payload.role,
            Some(BorrowedStr(Cow::Borrowed("user")))
        ));
        writeln!(file, "{mixed}").unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "response_item", "payload": {
                    "type": "message", "role": "assistant", "content": [
                        {"type": "output_text", "text": "assistant answer"},
                        {"type": "reasoning_text", "text": "exclude reasoning"}
                    ]
                }
            })
        )
        .unwrap();
        writeln!(file, "not json").unwrap();

        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "019c675d-0e41-7f71-925f-1ccc0da0f692");
        assert_eq!(found[0].cwd, PathBuf::from("/tmp/project"));
        assert_eq!(
            found[0].search_text,
            "<wallet>real request</wallet>\nfixture query\nquote \" and slash \\"
        );
        assert_eq!(found[0].preview, "<wallet>real request</wallet>");
        assert_eq!(
            serde_json::to_value(&found[0]).unwrap()["assistant_text"],
            "assistant answer"
        );
        assert_eq!(
            found[0].updated,
            parse_timestamp("2026-01-02T03:04:06.000Z").unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn excludes_internal_codex_threads() {
        let path =
            std::env::temp_dir().join(format!("codex-thread-kind-{}.jsonl", std::process::id()));
        for (kind, expected) in [
            ("guardian_review", false),
            ("subagent", false),
            ("user", true),
            ("realtime_voice", true),
        ] {
            let record = serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "019c675d-0e41-7f71-925f-1ccc0da0f692",
                    "cwd": "/tmp/project",
                    "thread_source": kind
                }
            });
            fs::write(&path, format!("{record}\n")).unwrap();
            assert_eq!(parse_file(&path).unwrap().is_some(), expected, "{kind}");
        }
        let malformed_source = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "019c675d-0e41-7f71-925f-1ccc0da0f692",
                "cwd": "/tmp/project",
                "thread_source": 7
            }
        });
        fs::write(&path, format!("{malformed_source}\n")).unwrap();
        assert!(parse_file(&path).unwrap().is_some());
        fs::remove_file(path).unwrap();
    }
}
