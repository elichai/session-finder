use crate::{vscode, BorrowedStr, Session, Source};
use serde::Deserialize;
use serde_json::{value::RawValue, Value};
use std::{fs, io, path::Path, time::UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot<'a> {
    #[serde(borrow)]
    requests: Vec<&'a RawValue>,
    creation_date: Option<i64>,
    #[serde(borrow)]
    custom_title: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    computed_title: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    working_directory: Option<BorrowedStr<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(borrow)]
    response: Option<&'a RawValue>,
    #[serde(borrow)]
    agent: Option<&'a RawValue>,
    timestamp: Option<i64>,
    response_timestamp: Option<i64>,
    #[serde(borrow)]
    model_state: Option<&'a RawValue>,
    #[serde(default)]
    hidden_from_transcript: bool,
    #[serde(default)]
    request_hidden_from_transcript: bool,
    #[serde(default)]
    is_system_initiated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Agent<'a> {
    #[serde(borrow)]
    id: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    extension_id: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Completion {
    completed_at: Option<i64>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    text: BorrowedStr<'a>,
}

#[derive(Deserialize)]
struct Markdown<'a> {
    #[serde(borrow)]
    kind: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    value: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct StringValue<'a> {
    #[serde(borrow)]
    value: BorrowedStr<'a>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Segment {
    Key(String),
    Index(usize),
}

#[derive(Deserialize)]
struct Entry<'a> {
    kind: u8,
    k: Option<Vec<Segment>>,
    #[serde(borrow, default, deserialize_with = "present_value")]
    v: Option<&'a RawValue>,
    i: Option<usize>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

pub fn parse_file(path: &Path) -> io::Result<Option<Session>> {
    let Some(id) = path
        .file_stem()
        .and_then(|id| id.to_str())
        .filter(|id| crate::codex::is_uuid(id))
    else {
        return Ok(None);
    };
    let journal = match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => false,
        Some("jsonl") => true,
        _ => return Ok(None),
    };
    Ok(
        parse_content(id, &fs::read(path)?, journal).map(|mut session| {
            if session.updated == 0 {
                session.updated = fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                    .and_then(|modified| i64::try_from(modified.as_secs()).ok())
                    .unwrap_or_default();
            }
            session
        }),
    )
}

fn parse_content(id: &str, bytes: &[u8], journal: bool) -> Option<Session> {
    let reconstructed;
    let bytes = if journal {
        reconstructed = serde_json::to_vec(&replay(bytes)?).ok()?;
        reconstructed.as_slice()
    } else {
        bytes
    };
    let snapshot: Snapshot<'_> = serde_json::from_slice(bytes).ok()?;
    let title = snapshot.custom_title.or(snapshot.computed_title);
    let mut context = vscode::Context {
        title: title.map_or_else(String::new, |title| title.0.into_owned()),
        format: if journal {
            vscode::StorageFormat::Journal
        } else {
            vscode::StorageFormat::Snapshot
        },
        ..Default::default()
    };
    let mut session = Session {
        source: Source::VsCode,
        id: id.to_owned(),
        cwd: snapshot
            .working_directory
            .as_deref()
            .and_then(vscode::file_uri_path)
            .unwrap_or_default(),
        updated: 0,
        search_text: String::new(),
        assistant_text: String::new(),
        preview: String::new(),
        archived: false,
        vscode: None,
    };
    let mut latest = snapshot.creation_date.unwrap_or_default().max(0);
    for raw in snapshot.requests {
        let Ok(request) = serde_json::from_str::<Request<'_>>(raw.get()) else {
            continue;
        };
        latest = latest
            .max(request.timestamp.unwrap_or_default())
            .max(request.response_timestamp.unwrap_or_default())
            .max(
                request
                    .model_state
                    .and_then(|raw| serde_json::from_str::<Completion>(raw.get()).ok())
                    .and_then(|state| state.completed_at)
                    .unwrap_or_default(),
            );
        if let Some(agent) = request
            .agent
            .and_then(|raw| serde_json::from_str::<Agent<'_>>(raw.get()).ok())
        {
            if let Some(id) = agent.id {
                participant(&mut context.participants, &id);
            }
            if let Some(raw) = agent.extension_id {
                if let Ok(id) = serde_json::from_str::<StringValue<'_>>(raw.get()) {
                    participant(&mut context.participants, &id.value);
                } else if let Ok(id) = serde_json::from_str::<BorrowedStr<'_>>(raw.get()) {
                    participant(&mut context.participants, &id);
                }
            }
        }
        if request.hidden_from_transcript {
            continue;
        }
        if !request.is_system_initiated && !request.request_hidden_from_transcript {
            if let Some(raw) = request.message {
                let message = serde_json::from_str::<BorrowedStr<'_>>(raw.get()).or_else(|_| {
                    serde_json::from_str::<Message<'_>>(raw.get()).map(|message| message.text)
                });
                if let Ok(message) = message {
                    if session.preview.is_empty() {
                        session.preview = crate::clean(&message, 200);
                    }
                    append(&mut session.search_text, &message);
                }
            }
        }
        if let Some(raw) = request.response {
            if let Ok(text) = serde_json::from_str::<BorrowedStr<'_>>(raw.get()) {
                append(&mut session.assistant_text, &text);
            } else if let Ok(parts) = serde_json::from_str::<Vec<&RawValue>>(raw.get()) {
                for part in parts {
                    response_part(&mut session.assistant_text, part);
                }
            }
        }
    }
    session.updated = latest / 1000;
    if session.preview.is_empty() {
        session.preview = crate::clean(&context.title, 200);
    }
    session.vscode = Some(context);
    Some(session)
}

fn append(text: &mut String, value: &str) {
    if !value.trim().is_empty() {
        text.push_str(value);
        text.push('\n');
    }
}

fn participant(participants: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !participants.iter().any(|item| item == value) {
        participants.push(value.to_owned());
    }
}

fn response_part(text: &mut String, raw: &RawValue) {
    if let Ok(value) = serde_json::from_str::<BorrowedStr<'_>>(raw.get()) {
        append(text, &value);
        return;
    }
    let Ok(part) = serde_json::from_str::<Markdown<'_>>(raw.get()) else {
        return;
    };
    match part.kind.as_deref() {
        None => {
            if let Some(value) = part.value {
                append(text, &value);
            }
        }
        Some("markdownContent" | "markdownVuln") => {
            if let Some(content) = part.content {
                if let Ok(markdown) = serde_json::from_str::<StringValue<'_>>(content.get()) {
                    append(text, &markdown.value);
                }
            }
        }
        _ => (),
    }
}

// VS Code's ObjectMutationLog (kind 0/1/2/3); dynamic JSON is confined to replay.
fn replay(bytes: &[u8]) -> Option<Value> {
    let mut state: Option<Value> = None;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry = match serde_json::from_slice::<Entry<'_>>(line) {
            Ok(entry) => entry,
            Err(error) if !line.ends_with(b"\n") && error.is_eof() => break,
            Err(_) => return None,
        };
        let value = entry
            .v
            .map(|raw| serde_json::from_str::<Value>(raw.get()))
            .transpose()
            .ok()?;
        if entry.kind == 0 {
            state = Some(value?);
            continue;
        }
        let state = state.as_mut()?;
        let path = entry.k?;
        // Native paths are shallow; bound trees assembled across many small mutations.
        if path.len() > 128 {
            return None;
        }
        let Some((last, parents)) = path.split_last() else {
            if matches!(entry.kind, 1 | 3) {
                continue; // The upstream reader ignores root set/delete entries.
            }
            return None;
        };
        let mut parent = state;
        for segment in parents {
            parent = match segment {
                Segment::Key(key) => parent.as_object_mut()?.get_mut(key)?,
                Segment::Index(index) => parent.as_array_mut()?.get_mut(*index)?,
            };
        }
        match entry.kind {
            1 | 3 => {
                let value = if entry.kind == 3 { None } else { value };
                match last {
                    Segment::Key(key) => {
                        let object = parent.as_object_mut()?;
                        if let Some(value) = value {
                            object.insert(key.clone(), value);
                        } else {
                            object.remove(key);
                        }
                    }
                    Segment::Index(index) => {
                        // JS deletion leaves an array hole; it does not splice later entries.
                        *parent.as_array_mut()?.get_mut(*index)? = value.unwrap_or(Value::Null);
                    }
                }
            }
            2 => {
                let array = match last {
                    Segment::Key(key) => parent
                        .as_object_mut()?
                        .entry(key.clone())
                        .or_insert(Value::Null),
                    Segment::Index(index) => parent.as_array_mut()?.get_mut(*index)?,
                };
                if array.is_null() {
                    *array = Value::Array(Vec::new());
                }
                let array = array.as_array_mut()?;
                if let Some(index) = entry.i {
                    if index > array.len() {
                        return None; // Generated logs truncate existing arrays, never create holes.
                    }
                    array.truncate(index);
                }
                if let Some(values) = value {
                    let Value::Array(values) = values else {
                        return None;
                    };
                    array.extend(values);
                }
            }
            _ => return None,
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;

    const ID: &str = "019c675d-0e41-7f71-925f-1ccc0da0f692";
    const SNAPSHOT: &str = r#"{
        "version":3,"sessionId":"different-id","creationDate":1700000000000,
        "customTitle":"Explain the mutex","workingDirectory":"file:///work/My%20Project",
        "inputState":{"inputText":"unsent draft"},
        "requests":[{
            "requestId":"turn1","timestamp":1700000001000,"responseTimestamp":1700000002000,
            "modelState":{"value":1,"completedAt":1700000003000},
            "message":{"text":"Why does this lock fail?","parts":[]},
            "agent":{"id":"vendor.assistant","extensionId":{"value":"Vendor.Extension"}},
            "response":[{"value":"The lock is held."},
                {"kind":"thinking","value":"private reasoning"},
                {"kind":"toolInvocationSerialized","value":"tool output","content":{"value":"secret"}},
                {"kind":"markdownVuln","content":{"value":"Check the scope."}}]
        }]
    }"#;

    #[test]
    fn snapshot_extracts_messages_and_separates_extension_metadata() {
        let session = parse_content(ID, SNAPSHOT.as_bytes(), false).unwrap();
        assert_eq!(session.source, Source::VsCode);
        assert_eq!(session.id, ID);
        assert_eq!(session.cwd, Path::new("/work/My Project"));
        assert_eq!(session.updated, 1700000003);
        assert_eq!(session.search_text, "Why does this lock fail?\n");
        assert_eq!(
            session.assistant_text,
            "The lock is held.\nCheck the scope.\n"
        );
        assert_eq!(session.preview, "Why does this lock fail?");
        let context = session.vscode.unwrap();
        assert_eq!(context.title, "Explain the mutex");
        assert_eq!(
            context.participants,
            ["vendor.assistant", "Vendor.Extension"]
        );
        assert_eq!(context.format, vscode::StorageFormat::Snapshot);
    }

    #[test]
    fn legacy_snapshot_accepts_strings_and_does_not_infer_a_remote_cwd() {
        let snapshot = br#"{"version":2,"computedTitle":"Old title","creationDate":1700000000000,
            "workingDirectory":"vscode-remote://ssh-remote+host/work",
            "requests":[{"message":"Opening question","response":"Old answer"},
            {"message":{"text":"Next question"},"response":["Second answer",{"value":"Third answer"}]}]}"#;
        let session = parse_content(ID, snapshot, false).unwrap();
        assert_eq!(session.search_text, "Opening question\nNext question\n");
        assert_eq!(
            session.assistant_text,
            "Old answer\nSecond answer\nThird answer\n"
        );
        assert!(session.cwd.as_os_str().is_empty());
        assert_eq!(session.updated, 1700000000);
        assert_eq!(session.vscode.unwrap().title, "Old title");
    }

    #[test]
    fn journal_replays_nested_sets_array_splices_and_deletes() {
        let journal = concat!(
            "{\"kind\":0,\"v\":{\"version\":3,\"creationDate\":1700000000000,\"customTitle\":\"stale\",\"requests\":[{\"message\":{\"text\":\"stale question\"},\"response\":[{\"value\":\"stale answer\"}]}]}}\n",
            "{\"kind\":1,\"k\":[\"requests\",0,\"message\",\"text\"],\"v\":\"Actual question\"}\n",
            "{\"kind\":2,\"k\":[\"requests\",0,\"response\"],\"i\":0,\"v\":[{\"value\":\"Actual answer\"},{\"value\":\"deleted\"}]}\n",
            "{\"kind\":2,\"k\":[\"requests\",0,\"response\"],\"i\":1}\n",
            "{\"kind\":2,\"k\":[\"requests\"],\"v\":[{\"message\":\"Follow up\",\"timestamp\":1700000004000,\"response\":[\"new answer\"]}]}\n",
            "{\"kind\":3,\"k\":[\"customTitle\"]}\n",
            "{\"kind\":1,\"k\":[\"requests\",1,\"response\"]}\n",
            "{\"kind\":1,\"k\":[\"requests\",0,\"response\",0,\"value\"],\"v\":\"Final answer\"}"
        );
        let session = parse_content(ID, journal.as_bytes(), true).unwrap();
        assert_eq!(session.search_text, "Actual question\nFollow up\n");
        assert_eq!(session.assistant_text, "Final answer\n");
        assert_eq!(session.updated, 1700000004);
        assert!(session.vscode.unwrap().title.is_empty());
    }

    #[test]
    fn journal_recovers_only_an_incomplete_unterminated_tail() {
        let snapshot: Value = serde_json::from_str(SNAPSHOT).unwrap();
        let initial = format!("{{\"kind\":0,\"v\":{snapshot}}}\n");
        let incomplete = format!("{initial}{{\"kind\":1,\"k\":[\"customTitle\"],\"v\":\"part");
        assert_eq!(
            parse_content(ID, incomplete.as_bytes(), true)
                .unwrap()
                .preview,
            "Why does this lock fail?"
        );
        for suffix in [
            "broken\n",
            "{\"kind\":1}\n",
            "{\"kind\":99}",
            "{\"kind\":1,\"k\":[\"missing\",0],\"v\":1}\n",
        ] {
            let corrupt = format!("{initial}{suffix}");
            assert!(
                parse_content(ID, corrupt.as_bytes(), true).is_none(),
                "{suffix}"
            );
        }
        let corrupt_middle =
            format!("{initial}broken\n{{\"kind\":1,\"k\":[\"customTitle\"],\"v\":\"later\"}}\n");
        assert!(parse_content(ID, corrupt_middle.as_bytes(), true).is_none());
        assert!(parse_content(ID, b"{\"kind\":0,\"v\":", true).is_none());
    }

    #[test]
    fn hidden_and_system_messages_do_not_become_human_search_text() {
        let snapshot = br#"{"requests":[
            {"message":"system prompt","isSystemInitiated":true,"response":[{"value":"Visible answer"}]},
            {"message":"hidden request","requestHiddenFromTranscript":true,"response":[{"value":"Another answer"}]},
            {"message":"hidden turn","hiddenFromTranscript":true,"response":[{"value":"hidden answer"}]},
            {"message":"Human question","response":[{"kind":"progressMessage","content":{"value":"progress"}}]}
        ]}"#;
        let session = parse_content(ID, snapshot, false).unwrap();
        assert_eq!(session.search_text, "Human question\n");
        assert_eq!(session.assistant_text, "Visible answer\nAnother answer\n");
        assert_eq!(session.preview, "Human question");
    }

    #[test]
    fn rejects_non_sessions_and_oversized_mutation_indexes_without_panicking() {
        for bytes in [b"{}".as_slice(), b"null", b"[]", b"{\"requests\":123}"] {
            assert!(parse_content(ID, bytes, false).is_none());
        }
        let snapshot: Value = serde_json::from_str(SNAPSHOT).unwrap();
        let journal = format!("{{\"kind\":0,\"v\":{snapshot}}}\n{{\"kind\":2,\"k\":[\"requests\"],\"i\":18446744073709551615}}\n");
        assert!(parse_content(ID, journal.as_bytes(), true).is_none());
        assert!(parse_content(
            ID,
            b"{\"kind\":1,\"k\":[\"title\"],\"v\":\"orphan\"}\n",
            true
        )
        .is_none());
    }

    #[test]
    fn replay_preserves_explicit_null_and_array_positions_when_deleting() {
        let journal = concat!(
            "{\"kind\":0,\"v\":{\"customTitle\":\"old\",\"requests\":[{\"message\":\"deleted\"},{\"message\":\"retained\"}]}}\n",
            "{\"kind\":1,\"k\":[\"customTitle\"],\"v\":null}\n",
            "{\"kind\":3,\"k\":[\"requests\",0]}\n",
            "{\"kind\":1,\"k\":[\"requests\",1,\"message\"],\"v\":\"Still second\"}\n"
        );
        let state = replay(journal.as_bytes()).unwrap();
        assert_eq!(state.get("customTitle"), Some(&Value::Null));
        assert_eq!(
            parse_content(ID, journal.as_bytes(), true)
                .unwrap()
                .search_text,
            "Still second\n"
        );
    }

    #[test]
    fn rejects_journals_that_build_unbounded_nesting_incrementally() {
        let mut journal = "{\"kind\":0,\"v\":{\"nested\":{}}}\n".to_owned();
        for depth in 1..=130 {
            let path = vec!["nested"; depth];
            journal.push_str(&serde_json::json!({"kind":1,"k":path,"v":{}}).to_string());
            journal.push('\n');
        }
        assert!(replay(journal.as_bytes()).is_none());
    }

    #[test]
    fn unfamiliar_optional_metadata_does_not_erase_messages() {
        let snapshot = br#"{"requests":[
            {"message":"First question","modelState":1,"response":[{"value":"First answer"}]},
            {"message":"Second question","agent":"legacy-agent","response":[{"value":"Second answer"}]}
        ]}"#;
        let session = parse_content(ID, snapshot, false).unwrap();
        assert_eq!(session.search_text, "First question\nSecond question\n");
        assert_eq!(session.assistant_text, "First answer\nSecond answer\n");
    }

    #[test]
    fn file_reader_validates_names_and_propagates_missing_file_errors() {
        assert!(parse_file(Path::new("not-a-uuid.json")).unwrap().is_none());
        assert!(parse_file(Path::new(&format!("{ID}.txt")))
            .unwrap()
            .is_none());
        let missing = Path::new("/missing/session-finder-vscode-native").join(format!("{ID}.json"));
        assert_eq!(
            parse_file(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
