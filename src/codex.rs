use crate::{BorrowedStr, Session, Source};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{
    fs,
    io::{BufRead, BufReader},
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
        .filter_map(|path| parse_file(&path))
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

pub fn parse_file(path: &Path) -> Option<Session> {
    let modified = fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let file = fs::File::open(path).ok()?;
    let mut id = None;
    let mut cwd = None;
    let mut updated = None;
    let mut search_text = String::new();
    let mut preview = String::new();

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(record) = serde_json::from_str::<CodexRecord<'_>>(&line) else {
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
                return None;
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
            if payload.kind.as_deref() != Some("message") || payload.role.as_deref() != Some("user")
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
                    if block.kind.as_deref() == Some("input_text") {
                        if let Some(text) = block.text {
                            let candidate = text.trim_start();
                            // Codex records injected context as user-role messages.
                            if candidate.starts_with("# AGENTS.md instructions for ")
                                || candidate.starts_with("<environment_context>")
                                || candidate.starts_with("<external_codex_apps_")
                            {
                                continue;
                            }
                            if preview.is_empty() && !candidate.is_empty() {
                                preview = candidate.lines().next().unwrap_or_default().to_owned();
                            }
                            if !search_text.is_empty() {
                                search_text.push('\n');
                            }
                            search_text.push_str(&text);
                        }
                    }
                }
            }
        }
    }

    Some(Session {
        source: Source::Codex,
        id: id?,
        cwd: cwd?,
        updated: updated.unwrap_or(modified),
        preview: if preview.is_empty() {
            search_text.lines().next().unwrap_or_default().to_owned()
        } else {
            preview
        },
        search_text,
        archived: false,
    })
}

pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn parse_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        std::str::from_utf8(bytes.get(range)?).ok()?.parse().ok()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=month_days).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?;
    if bytes.get(19) == Some(&b'.') {
        let mut i = 20;
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
        let zone = std::str::from_utf8(bytes.get(i..)?).ok()?;
        if zone != "Z" {
            return None;
        }
    } else if bytes.get(19) != Some(&b'Z') {
        return None;
    }
    Some(seconds)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let year = year.checked_sub(i64::from(month <= 2))?;
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::io::Write;

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
            assert_eq!(parse_file(&path).is_some(), expected, "{kind}");
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
        assert!(parse_file(&path).is_some());
        fs::remove_file(path).unwrap();
    }
}
