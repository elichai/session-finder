mod claude;
mod codex;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum Source {
    Claude,
    Codex,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Session {
    pub source: Source,
    pub id: String,
    pub cwd: PathBuf,
    pub updated: i64,
    pub search_text: String,
    pub preview: String,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Deserialize, Serialize)]
struct CacheEntry {
    modified_ns: u128,
    len: u64,
    session: Option<Session>,
}

#[derive(Default, Deserialize, Serialize)]
struct Cache {
    version: u8,
    entries: HashMap<String, CacheEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectionAction {
    Copy,
    OpenGui,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("session-finder: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("Usage: session-finder [--fuzzy] [--list | --print SESSION_ID | --rebuild]\n\nOpen a recent-first search panel of local Claude and Codex sessions.\nSearch matches the typed phrase; --fuzzy enables loose subsequence matching.\nEnter copies and prints the exact resume command. Ctrl-O opens a Codex chat in the GUI. Esc cancels.\n\nSESSION_FINDER_CACHE sets the private cache file path.\nSESSION_FINDER_CLAUDE_ROOT, SESSION_FINDER_CODEX_ROOT, and\nSESSION_FINDER_CODEX_ARCHIVE_ROOT override history roots.");
        return Ok(());
    }
    let rebuild = args.iter().any(|arg| arg == "--rebuild");
    let fuzzy = args.iter().any(|arg| arg == "--fuzzy");
    let list = args.iter().any(|arg| arg == "--list");
    let print_id = args
        .iter()
        .position(|arg| arg == "--print")
        .and_then(|i| args.get(i + 1));
    if args.iter().any(|arg| {
        !matches!(arg.as_str(), "--list" | "--print" | "--rebuild" | "--fuzzy")
            && Some(arg) != print_id
    }) {
        return Err("unknown argument; use --help".into());
    }
    if args.iter().any(|arg| arg == "--print") && print_id.is_none() {
        return Err("--print needs a session ID".into());
    }

    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?);
    let claude_root = std::env::var_os("SESSION_FINDER_CLAUDE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude/projects"));
    let codex_root = std::env::var_os("SESSION_FINDER_CODEX_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex/sessions"));
    let codex_archive_root = std::env::var_os("SESSION_FINDER_CODEX_ARCHIVE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex/archived_sessions"));
    let cache_path = std::env::var_os("SESSION_FINDER_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache/session-finder/index.json"));
    let sessions = load_sessions(
        &claude_root,
        &codex_root,
        &codex_archive_root,
        &cache_path,
        rebuild,
    )?;
    if list {
        for session in &sessions {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                session.updated,
                display_source(session),
                session.id,
                session.cwd.display(),
                clean(&session.preview, 100)
            );
        }
        return Ok(());
    }
    if let Some(id) = print_id {
        let matches: Vec<_> = sessions
            .iter()
            .filter(|session| session.id == *id)
            .collect();
        if matches.len() != 1 {
            return Err(format!("expected one session for {id}, found {}", matches.len()).into());
        }
        warn_if_missing(matches[0]);
        println!("{}", resume_command(matches[0]));
        return Ok(());
    }
    if sessions.is_empty() {
        return Err("no Claude or Codex sessions found".into());
    }
    let Some((selected, action)) = choose(&sessions, fuzzy)? else {
        return Ok(());
    };
    let session = &sessions[selected];
    if action == SelectionAction::OpenGui {
        let url = gui_url(session)?;
        let status = Command::new("open").arg(&url).status()?;
        if !status.success() {
            return Err(format!("could not open Codex GUI: {status}").into());
        }
        println!("Opened {url}");
        return Ok(());
    }
    warn_if_missing(session);
    let command = resume_command(session);
    let copied = match Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        Ok(mut child) => {
            let written = child
                .stdin
                .take()
                .is_some_and(|mut stdin| stdin.write_all(command.as_bytes()).is_ok());
            let exited = child.wait().is_ok_and(|status| status.success());
            written && exited
        }
        Err(_) => false,
    };
    if copied {
        eprintln!("Copied resume command to clipboard.");
    } else {
        eprintln!("Clipboard unavailable; command is printed below.");
    }
    println!("{command}");
    Ok(())
}

fn load_sessions(
    claude_root: &Path,
    codex_root: &Path,
    codex_archive_root: &Path,
    cache_path: &Path,
    rebuild: bool,
) -> io::Result<Vec<Session>> {
    let debug = std::env::var_os("SESSION_FINDER_DEBUG").is_some();
    let started = Instant::now();
    let old = if rebuild {
        Cache::default()
    } else {
        read_cache(cache_path)
    };
    if old.version != 1 {
        eprintln!("Indexing local history; the first run may take a few seconds...");
    }
    let mut previous = old.entries;
    let mut next = HashMap::new();
    let mut changed = 0;
    for (path, parse, archived) in claude::session_files(claude_root)
        .into_iter()
        .map(|path| {
            (
                path,
                claude::parse_file as fn(&Path) -> Option<Session>,
                false,
            )
        })
        .chain(codex::session_files(codex_root).into_iter().map(|path| {
            (
                path,
                codex::parse_file as fn(&Path) -> Option<Session>,
                false,
            )
        }))
        .chain(
            codex::session_files(codex_archive_root)
                .into_iter()
                .map(|path| {
                    (
                        path,
                        codex::parse_file as fn(&Path) -> Option<Session>,
                        true,
                    )
                }),
        )
    {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let modified_ns = meta
            .modified()
            .ok()
            .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos());
        let key = path.to_string_lossy().into_owned();
        let entry = match previous.remove(&key) {
            Some(entry) if entry.modified_ns == modified_ns && entry.len == meta.len() => entry,
            _ => {
                changed += 1;
                let parse_started = Instant::now();
                let session = parse(&path).map(|mut session| {
                    session.archived = archived;
                    session
                });
                if debug {
                    eprintln!(
                        "parsed {} bytes in {:?}: {}",
                        meta.len(),
                        parse_started.elapsed(),
                        path.display()
                    );
                }
                CacheEntry {
                    modified_ns,
                    len: meta.len(),
                    session,
                }
            }
        };
        next.insert(key, entry);
    }
    let removed = !previous.is_empty();
    let sessions: Vec<Session> = next
        .values()
        .filter_map(|entry| entry.session.clone())
        .collect();
    let sessions = merge_sessions(sessions);
    if changed > 0 || removed || old.version != 1 {
        let write_started = Instant::now();
        write_cache(
            cache_path,
            &Cache {
                version: 1,
                entries: next,
            },
        )?;
        if debug {
            eprintln!(
                "wrote cache in {:?}; total {:?}",
                write_started.elapsed(),
                started.elapsed()
            );
        }
        if changed > 0 {
            eprintln!("Indexed {changed} changed session files.");
        }
    }
    if debug {
        eprintln!(
            "loaded {} sessions in {:?}",
            sessions.len(),
            started.elapsed()
        );
    }
    Ok(sessions)
}

fn merge_sessions(parts: Vec<Session>) -> Vec<Session> {
    let mut merged: HashMap<(Source, String), Session> = HashMap::new();
    for mut part in parts {
        let key = (part.source, part.id.clone());
        match merged.get_mut(&key) {
            None => {
                merged.insert(key, part);
            }
            Some(existing) => {
                if part.updated > existing.updated {
                    std::mem::swap(existing, &mut part);
                }
                existing.archived &= part.archived;
                if !part.search_text.is_empty() && !existing.search_text.contains(&part.search_text)
                {
                    if !existing.search_text.is_empty() {
                        existing.search_text.push('\n');
                    }
                    existing.search_text.push_str(&part.search_text);
                }
            }
        }
    }
    let mut sessions: Vec<_> = merged.into_values().collect();
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    sessions
}

fn read_cache(path: &Path) -> Cache {
    let Ok(bytes) = fs::read(path) else {
        return Cache::default();
    };
    match serde_json::from_slice::<Cache>(&bytes) {
        Ok(cache) if cache.version == 1 => cache,
        _ => Cache::default(),
    }
}

fn write_cache(path: &Path, cache: &Cache) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = path.with_extension(format!("tmp-{}-{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec(cache)?)?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(tmp, path)
}

fn choose(sessions: &[Session], fuzzy: bool) -> io::Result<Option<(usize, SelectionAction)>> {
    let mut picker = Command::new("fzf");
    picker.args([
        "--delimiter=\t",
        "--with-nth=2..",
        "--accept-nth=1",
        "--expect=ctrl-o",
        "--no-sort",
        "--freeze-left=2",
        "--layout=reverse",
        "--border",
        "--height=90%",
        "--prompt=Sessions> ",
        "--header=Enter: copy • Ctrl-O: Codex GUI • Esc: close • Codex*: archived",
    ]);
    if !fuzzy {
        picker.args(["--exact", "--no-extended"]);
    }
    let mut child = picker
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        for (i, session) in sessions.iter().enumerate() {
            let age = age(session.updated);
            let cwd = display_cwd(&session.cwd);
            let full_cwd = clean(&session.cwd.to_string_lossy(), usize::MAX);
            let preview = clean(&session.preview, 60);
            let search_text = clean(&session.search_text, usize::MAX);
            if writeln!(
                stdin,
                "{i}\t{age:>8}  {:<6}  {cwd:<28}\t{preview:<60}\t{search_text}\t{}\t{}",
                display_source(session),
                full_cwd,
                session.id
            )
            .is_err()
            {
                break;
            }
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Ok(None);
    }
    parse_selection(&output.stdout, sessions.len()).map(Some)
}

fn parse_selection(output: &[u8], count: usize) -> io::Result<(usize, SelectionAction)> {
    let output = String::from_utf8_lossy(output);
    let mut lines = output.lines();
    let action = match lines.next() {
        Some("") => SelectionAction::Copy,
        Some("ctrl-o") => SelectionAction::OpenGui,
        _ => return Err(io::Error::other("fzf returned an unknown action")),
    };
    let index = lines
        .next()
        .ok_or_else(|| io::Error::other("fzf returned no selection"))?
        .parse::<usize>()
        .map_err(io::Error::other)?;
    if index >= count {
        return Err(io::Error::other("fzf returned an invalid selection"));
    }
    Ok((index, action))
}

fn gui_url(session: &Session) -> Result<String, &'static str> {
    if session.source != Source::Codex {
        return Err("Codex GUI can only open Codex sessions");
    }
    if !codex::is_uuid(&session.id) {
        return Err("Codex session has an invalid ID");
    }
    Ok(format!("codex://threads/{}", session.id))
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::Claude => "Claude",
        Source::Codex => "Codex",
    }
}

fn display_source(session: &Session) -> &'static str {
    if session.archived {
        "Codex*"
    } else {
        source_name(session.source)
    }
}

fn age(updated: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let seconds = now.saturating_sub(updated).max(0);
    if seconds < 60 {
        "now".into()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn clean(value: &str, limit: usize) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn display_cwd(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let text = match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(relative) if relative.as_os_str().is_empty() => "~".to_owned(),
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    };
    let text = clean(&text, usize::MAX);
    let len = text.chars().count();
    if len > 28 {
        format!("…{}", text.chars().skip(len - 27).collect::<String>())
    } else {
        text
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn resume_command(session: &Session) -> String {
    let cwd = shell_quote(&session.cwd.to_string_lossy());
    let id = shell_quote(&session.id);
    match session.source {
        Source::Claude => format!("cd -- {cwd} && claude --resume {id}"),
        Source::Codex => format!("codex resume {id} -C {cwd}"),
    }
}

fn warn_if_missing(session: &Session) {
    if !session.cwd.is_dir() {
        eprintln!(
            "Recorded working directory is missing: {}",
            session.cwd.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_command_quotes_untrusted_cwd() {
        let session = Session {
            source: Source::Claude,
            id: "abc".into(),
            cwd: PathBuf::from("/tmp/O'Brien; echo bad"),
            updated: 0,
            search_text: String::new(),
            preview: String::new(),
            archived: false,
        };
        assert_eq!(
            resume_command(&session),
            "cd -- '/tmp/O'\\''Brien; echo bad' && claude --resume 'abc'"
        );
    }

    #[test]
    fn resumed_rollouts_form_one_searchable_session() {
        let parts = vec![
            Session {
                source: Source::Codex,
                id: "same".into(),
                cwd: "/old".into(),
                updated: 10,
                search_text: "earlier words".into(),
                preview: "earlier".into(),
                archived: false,
            },
            Session {
                source: Source::Codex,
                id: "same".into(),
                cwd: "/new".into(),
                updated: 20,
                search_text: "later words".into(),
                preview: "later".into(),
                archived: false,
            },
        ];
        let merged = merge_sessions(parts);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].cwd, PathBuf::from("/new"));
        assert!(merged[0].search_text.contains("earlier words"));
        assert!(merged[0].search_text.contains("later words"));
    }

    #[test]
    fn archived_codex_resumes_by_id() {
        let session = Session {
            source: Source::Codex,
            id: "abc".into(),
            cwd: "/work".into(),
            updated: 1,
            search_text: String::new(),
            preview: String::new(),
            archived: true,
        };
        assert_eq!(resume_command(&session), "codex resume 'abc' -C '/work'");
    }

    #[test]
    fn picker_distinguishes_copy_from_gui_open() {
        assert_eq!(
            parse_selection(b"\n7\n", 8).unwrap(),
            (7, SelectionAction::Copy)
        );
        assert_eq!(
            parse_selection(b"ctrl-o\n7\n", 8).unwrap(),
            (7, SelectionAction::OpenGui)
        );
        assert!(parse_selection(b"ctrl-o\n8\n", 8).is_err());
    }

    #[test]
    fn gui_link_targets_only_codex_sessions() {
        let mut session = Session {
            source: Source::Codex,
            id: "019c675d-0e41-7f71-925f-1ccc0da0f692".into(),
            cwd: "/work".into(),
            updated: 0,
            search_text: String::new(),
            preview: String::new(),
            archived: false,
        };
        assert_eq!(
            gui_url(&session).unwrap(),
            "codex://threads/019c675d-0e41-7f71-925f-1ccc0da0f692"
        );
        session.source = Source::Claude;
        assert!(gui_url(&session).is_err());
        session.source = Source::Codex;
        session.id = "../new".into();
        assert!(gui_url(&session).is_err());
    }
}
