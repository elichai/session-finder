use super::{Session, Source};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Deserialize)]
struct ClaudeRow<'a> {
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    cwd: Option<&'a RawValue>,
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
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
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

#[cfg(test)]
pub fn scan(root: &Path) -> Vec<Session> {
    let mut sessions: Vec<_> = session_files(root)
        .iter()
        .filter_map(|path| parse_file(path))
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

pub fn parse_file(path: &Path) -> Option<Session> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let mut cwd = None;
    let mut search_text = String::new();
    let mut preview = String::new();
    let mut updated = 0;
    let file = fs::File::open(path).ok()?;
    let mut genuine = false;
    for line in BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { continue };
        let Ok(row) = serde_json::from_slice::<ClaudeRow<'_>>(&line) else {
            continue;
        };
        if row.session_id.as_deref() != Some(id.as_str()) {
            continue;
        }
        if let Some(dir) = row
            .cwd
            .and_then(|raw| serde_json::from_str::<Cow<'_, str>>(raw.get()).ok())
        {
            cwd.get_or_insert_with(|| PathBuf::from(dir.as_ref()));
        }
        if let Some(ts) = row
            .timestamp
            .and_then(|raw| serde_json::from_str::<Cow<'_, str>>(raw.get()).ok())
            .and_then(|text| parse_timestamp(&text))
        {
            updated = updated.max(ts);
        }
        if row.kind.as_deref() != Some("user") {
            continue;
        }
        genuine = true;
        let Some(message) = row.message else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<ClaudeMessage<'_>>(message.get()) else {
            continue;
        };
        if let Some(text) = user_text(message.content) {
            if !text.trim().is_empty() {
                if preview.is_empty() {
                    preview = text.chars().take(240).collect();
                }
                if !search_text.is_empty() {
                    search_text.push('\n');
                }
                search_text.push_str(&text);
            }
        }
    }
    if !genuine {
        return None;
    }
    let cwd = cwd?;
    if updated == 0 {
        updated = fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
    }
    Some(Session {
        source: Source::Claude,
        id,
        cwd,
        updated,
        search_text,
        preview,
        archived: false,
    })
}

fn user_text(value: Option<&RawValue>) -> Option<String> {
    let raw = value?.get();
    if let Ok(text) = serde_json::from_str::<Cow<'_, str>>(raw) {
        return Some(text.into_owned());
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

// Claude timestamps are RFC3339. Convert UTC calendar dates without a date crate.
fn parse_timestamp(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return s.parse().ok();
    }
    let number = |range: std::ops::Range<usize>| s.get(range)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (
        number(0..4)?,
        number(5..7)?,
        number(8..10)?,
        number(11..13)?,
        number(14..16)?,
        number(17..19)?,
    );
    let days = days_from_civil(y, m, d)?;
    let mut epoch = days.checked_mul(86_400)? + hh * 3600 + mm * 60 + ss;
    if bytes.get(19) == Some(&b'.') {
        let mut i = 20;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if let Some(zone) = s.get(i..) {
            epoch -= zone_offset(zone)?;
        }
    } else if let Some(zone) = s.get(19..) {
        epoch -= zone_offset(zone)?;
    }
    Some(epoch)
}

fn zone_offset(zone: &str) -> Option<i64> {
    if zone == "Z" {
        return Some(0);
    }
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hh = zone.get(1..3)?.parse::<i64>().ok()?;
    let mm = zone.get(4..6)?.parse::<i64>().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    Some(era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "{{\"type\":\"user\",\"sessionId\":\"{id}\",\"cwd\":\"/work\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"find this phrase\"}}]}}}}\nnot-json\n"
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
