use crate::{json_lines::JsonLines, session_files::SessionFiles, BorrowedStr, Session, Source};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Deserialize)]
struct Event<'a> {
    #[serde(rename = "type", borrow)]
    kind: BorrowedStr<'a>,
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    #[serde(rename = "agentId", borrow)]
    agent_id: Option<&'a RawValue>,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(Deserialize)]
struct Metadata<'a> {
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<BorrowedStr<'a>>,
    #[serde(borrow)]
    context: Option<Context<'a>>,
}

#[derive(Deserialize)]
struct Context<'a> {
    #[serde(borrow)]
    cwd: Option<BorrowedStr<'a>>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    content: BorrowedStr<'a>,
    #[serde(borrow)]
    source: Option<BorrowedStr<'a>>,
    #[serde(rename = "parentToolCallId", borrow)]
    parent_tool_call_id: Option<&'a RawValue>,
    #[serde(rename = "isAutopilotContinuation", default)]
    autopilot: bool,
}

pub fn session_files(root: &Path) -> SessionFiles {
    SessionFiles::new(root, |path| {
        path.file_name().is_some_and(|name| name == "events.jsonl")
    })
}

pub fn parse_file(path: &Path) -> io::Result<Option<Session>> {
    let Some(id) = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|id| id.to_str())
    else {
        return Ok(None);
    };
    if id.is_empty() || id.chars().any(char::is_control) {
        return Ok(None);
    }
    let modified = fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs() as i64;
    parse_reader(id, BufReader::new(fs::File::open(path)?), modified)
}

fn parse_reader(id: &str, reader: impl BufRead, modified: i64) -> io::Result<Option<Session>> {
    let mut session = Session {
        source: Source::Copilot,
        id: id.to_owned(),
        cwd: PathBuf::new(),
        updated: modified,
        search_text: String::new(),
        assistant_text: String::new(),
        preview: String::new(),
        archived: false,
    };
    let mut started = false;
    let mut updated = None;
    let mut lines = JsonLines::new(reader);
    while let Some(line) = lines.next_line()? {
        let Ok(event) = serde_json::from_slice::<Event<'_>>(line) else {
            continue;
        };
        if event.agent_id.is_some() {
            continue;
        }
        if let Some(timestamp) = event
            .timestamp
            .and_then(|raw| serde_json::from_str::<BorrowedStr<'_>>(raw.get()).ok())
            .and_then(|text| crate::timestamp::parse(&text))
        {
            updated = Some(updated.map_or(timestamp, |last: i64| last.max(timestamp)));
        }
        match &*event.kind {
            "session.start" | "session.resume" => {
                let Ok(meta) = serde_json::from_str::<Metadata<'_>>(event.data.get()) else {
                    continue;
                };
                if &*event.kind == "session.start" {
                    if meta.session_id.as_deref() != Some(id) {
                        return Ok(None);
                    }
                    started = true;
                }
                if let Some(cwd) = meta.context.and_then(|context| context.cwd) {
                    if Path::new(&*cwd).is_absolute() {
                        session.cwd = PathBuf::from(&*cwd);
                    }
                }
            }
            "session.context_changed" => {
                let Ok(context) = serde_json::from_str::<Context<'_>>(event.data.get()) else {
                    continue;
                };
                if let Some(cwd) = context.cwd {
                    if Path::new(&*cwd).is_absolute() {
                        session.cwd = PathBuf::from(&*cwd);
                    }
                }
            }
            "user.message" | "assistant.message" => {
                let Ok(message) = serde_json::from_str::<Message<'_>>(event.data.get()) else {
                    continue;
                };
                if message.parent_tool_call_id.is_some()
                    || message.autopilot
                    || message.content.trim().is_empty()
                {
                    continue;
                }
                let assistant = &*event.kind == "assistant.message";
                if !assistant
                    && message
                        .source
                        .as_deref()
                        .is_some_and(|source| source != "user")
                {
                    continue;
                }
                if !assistant && session.preview.is_empty() {
                    session.preview = message.content.chars().take(240).collect();
                }
                let target = if assistant {
                    &mut session.assistant_text
                } else {
                    &mut session.search_text
                };
                if !target.is_empty() {
                    target.push('\n');
                }
                target.push_str(&message.content);
            }
            _ => (),
        }
    }
    session.updated = updated.unwrap_or(modified);
    Ok(
        (started && !session.cwd.as_os_str().is_empty() && !session.search_text.is_empty())
            .then_some(session),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"task\",\"context\":{\"cwd\":\"/old\"}}}\n";
    const USER: &str = "{\"type\":\"user.message\",\"data\":{\"content\":\"request\"}}\n";

    #[test]
    fn resumes_update_cwd_and_missing_timestamps_fall_back_to_file_time() {
        let text = [
            START,
            USER,
            "{\"type\":\"session.resume\",\"data\":{\"context\":{\"cwd\":\"/new\"}}}\n",
        ]
        .concat();
        let session = parse_reader("task", text.as_bytes(), 123).unwrap().unwrap();
        assert_eq!(session.cwd, Path::new("/new"));
        assert_eq!(session.updated, 123);
    }

    #[test]
    fn skips_wrong_identity_drafts_missing_metadata_and_relative_directories() {
        for text in [
            START.to_owned(),
            USER.to_owned(),
            [START, USER].concat().replace("\"task\"", "\"other\""),
            [START, USER].concat().replace("/old", "relative"),
            [START, USER].concat().replace("/old", ""),
            [START, USER]
                .concat()
                .replace("\"cwd\":\"/old\"", "\"cwd\":7"),
        ] {
            assert!(
                parse_reader("task", text.as_bytes(), 123)
                    .unwrap()
                    .is_none(),
                "{text}"
            );
        }
    }

    #[test]
    fn read_errors_do_not_return_a_partial_session() {
        let text = [START, USER].concat();
        let reader = crate::json_lines::reader_failing_after(text.as_bytes());
        assert_eq!(
            parse_reader("task", reader, 123).unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }
}
