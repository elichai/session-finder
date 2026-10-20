mod args;
mod claude;
mod codex;
mod copilot;
mod cursor;
mod json_lines;
mod picker;
mod session_files;
mod timestamp;
mod vscode;
mod vscode_native;
mod vscode_open;
mod vscode_remote;

use args::{Action, Args};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const CACHE_VERSION: u8 = 6;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum Source {
    Claude,
    Codex,
    Copilot,
    Cursor,
    VsCode,
}

#[derive(Deserialize)]
pub(crate) struct BorrowedStr<'a>(#[serde(borrow)] pub(crate) Cow<'a, str>);

impl std::ops::Deref for BorrowedStr<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Session {
    pub source: Source,
    pub id: String,
    pub cwd: PathBuf,
    pub updated: i64,
    pub search_text: String,
    pub assistant_text: String,
    pub preview: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub vscode: Option<vscode::Context>,
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

struct History {
    claude_root: PathBuf,
    codex_root: PathBuf,
    codex_archive_root: PathBuf,
    copilot_root: PathBuf,
    vscode_roots: Vec<PathBuf>,
    cursor_db: PathBuf,
    cache_path: PathBuf,
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
    let args = Args::parse(std::env::args().skip(1))?;
    match &args.action {
        Action::PickerHeader => {
            picker::print_header();
            return Ok(());
        }
        Action::PickerPreview { path, fuzzy, query } => {
            return Ok(picker::print_preview(path, query, *fuzzy)?);
        }
        _ => (),
    }
    if args.action == Action::Help {
        print!(concat!(
            "Usage: session-finder [--fuzzy] [--list | --print SESSION_ID | --open SESSION_ID] [--rebuild]\n\n",
            "Search local Claude, Codex, Copilot CLI, VS Code chat, and Cursor sessions, most recent first.\n",
            "VS Code includes Copilot and other native Chat participants.\n",
            "Search matches the typed phrase; --fuzzy enables loose subsequence matching.\n",
            "Both modes use literal characters and smart case, without extended-search operators.\n",
            "Enter copies the resume command. Ctrl-O opens a supported app session. Esc cancels.\n",
            "Copilot CLI sessions resume in a terminal; use Enter or --print.\n",
            "VS Code native chats open in their recorded workspace, including saved .code-workspace files.\n",
            "Enter copies a resume command; Ctrl-O opens the exact native chat without a helper extension.\n",
            "Recovery-only transcripts and unknown workspaces retain workspace-opening commands.\n",
            "VS Code titles, workspace targets, and participant IDs are always searchable.\n",
            "Ctrl-A toggles assistant-answer search (off initially). Ctrl-R refreshes history.\n",
            "The bottom pane shows a highlighted matching passage. Requires fzf 0.71.0 or later.\n",
            "Assistant answers are always indexed in the private plaintext cache.\n",
            "Cursor opening requires the local Session Finder helper extension.\n\n",
            "SESSION_FINDER_CACHE sets the private cache file path.\n",
            "SESSION_FINDER_CLAUDE_ROOT, SESSION_FINDER_CODEX_ROOT, and\n",
            "SESSION_FINDER_CODEX_ARCHIVE_ROOT override history roots.\n",
            "SESSION_FINDER_COPILOT_ROOT overrides Copilot's session-state directory.\n",
            "Otherwise Copilot uses $COPILOT_HOME/session-state or ~/.copilot/session-state.\n",
            "SESSION_FINDER_VSCODE_ROOT sets VS Code workspaceStorage directories (colon-separated).\n",
            "Otherwise VS Code uses desktop, Insiders, and Remote server locations under $HOME.\n",
            "The sibling globalStorage/emptyWindowChatSessions directory is also read.\n",
            "SESSION_FINDER_CURSOR_DB overrides Cursor's local history database.\n",
        ));
        return Ok(());
    }

    let history = History::from_env()?;
    let sessions = history.load(args.rebuild)?;
    if args.action == Action::PickerRows {
        picker::write_rows(&sessions, &mut io::stdout().lock())?;
        return Ok(());
    }
    if args.action == Action::List {
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
    if let Action::Print(id) | Action::Open(id) = &args.action {
        let qualified = id.contains(':')
            && sessions
                .iter()
                .any(|session| picker::identity(session) == *id);
        let mut matches = sessions.iter().filter(|session| {
            if qualified {
                picker::identity(session) == *id
            } else {
                session.id == *id
            }
        });
        let session = match (matches.next(), matches.count()) {
            (Some(session), 0) => session,
            (first, remaining) => {
                let count = usize::from(first.is_some()) + remaining;
                return Err(format!("expected one session for {id}, found {count}").into());
            }
        };
        if matches!(args.action, Action::Open(_)) {
            return open_gui(session);
        }
        warn_if_missing(session);
        println!("{}", resume_command(session));
        return Ok(());
    }
    if sessions.is_empty() {
        return Err("no Claude, Codex, Copilot CLI, VS Code chat, or Cursor sessions found".into());
    }
    let Some((selected, action)) = picker::choose(&sessions, args.fuzzy)? else {
        return Ok(());
    };
    // Refresh runs in a picker child, so resolve its stable identity against fresh history.
    let sessions = history.load(false)?;
    let session = sessions
        .iter()
        .find(|session| picker::identity(session) == selected)
        .ok_or("selected session no longer exists; reopen the picker")?;
    if action == SelectionAction::OpenGui {
        return open_gui(session);
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

impl History {
    fn from_env() -> Result<Self, &'static str> {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?);
        let path = |variable, default| {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(default))
        };
        Ok(Self {
            claude_root: path("SESSION_FINDER_CLAUDE_ROOT", ".claude/projects"),
            codex_root: path("SESSION_FINDER_CODEX_ROOT", ".codex/sessions"),
            codex_archive_root: path(
                "SESSION_FINDER_CODEX_ARCHIVE_ROOT",
                ".codex/archived_sessions",
            ),
            copilot_root: std::env::var_os("SESSION_FINDER_COPILOT_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| path("COPILOT_HOME", ".copilot").join("session-state")),
            vscode_roots: match std::env::var_os("SESSION_FINDER_VSCODE_ROOT") {
                Some(roots) => std::env::split_paths(&roots).collect(),
                None => vscode::DEFAULT_ROOTS
                    .iter()
                    .map(|root| home.join(root))
                    .collect(),
            },
            cursor_db: path(
                "SESSION_FINDER_CURSOR_DB",
                "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
            ),
            cache_path: path("SESSION_FINDER_CACHE", ".cache/session-finder/index.json"),
        })
    }

    fn load(&self, rebuild: bool) -> io::Result<Vec<Session>> {
        let debug = std::env::var_os("SESSION_FINDER_DEBUG").is_some();
        let started = Instant::now();
        let old = if rebuild {
            Cache::default()
        } else {
            read_cache(&self.cache_path)
        };
        if old.version != CACHE_VERSION {
            eprintln!("Indexing local history; the first run may take a few seconds...");
        }
        let mut previous = old.entries;
        let mut next = HashMap::with_capacity(previous.len());
        let mut changed = 0;
        let mut roots = vec![
            (
                &self.claude_root,
                claude::session_files as fn(&Path) -> session_files::SessionFiles,
                claude::parse_file as fn(&Path) -> io::Result<Option<Session>>,
                false,
            ),
            (
                &self.codex_root,
                codex::session_files,
                codex::parse_file,
                false,
            ),
            (
                &self.codex_archive_root,
                codex::session_files,
                codex::parse_file,
                true,
            ),
            (
                &self.copilot_root,
                copilot::session_files,
                copilot::parse_file,
                false,
            ),
        ];
        roots.extend(self.vscode_roots.iter().map(|root| {
            (
                root,
                vscode::session_files as _,
                vscode::parse_file as _,
                false,
            )
        }));
        for (root, files, parse, archived) in roots {
            for path in files(root) {
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
                    Some(entry) if entry.modified_ns == modified_ns && entry.len == meta.len() => {
                        entry
                    }
                    _ => {
                        changed += 1;
                        let parse_started = Instant::now();
                        let session = match parse(&path) {
                            Ok(session) => session,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => {
                                return Err(io::Error::new(
                                    error.kind(),
                                    format!("could not read {}: {error}", path.display()),
                                ));
                            }
                        }
                        .map(|mut session| {
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
        }
        let removed = !previous.is_empty();
        let cache = Cache {
            version: CACHE_VERSION,
            entries: next,
        };
        if changed > 0 || removed || old.version != CACHE_VERSION {
            let write_started = Instant::now();
            write_cache(&self.cache_path, &cache)?;
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
        let cursor_sessions = match cursor::scan(&self.cursor_db) {
            Ok(sessions) => sessions,
            Err(error) => {
                eprintln!("Could not read Cursor history: {error}");
                Vec::new()
            }
        };
        let sessions = merge_sessions(
            cache
                .entries
                .into_iter()
                .filter_map(|(path, entry)| {
                    entry.session.map(|mut session| {
                        if session.source == Source::VsCode {
                            vscode::resolve_context(Path::new(&path), &mut session);
                        }
                        session
                    })
                })
                .chain(cursor_sessions),
        );
        if debug {
            eprintln!(
                "loaded {} sessions in {:?}",
                sessions.len(),
                started.elapsed()
            );
        }
        Ok(sessions)
    }
}

fn merge_sessions(parts: impl IntoIterator<Item = Session>) -> Vec<Session> {
    let parts = parts.into_iter();
    let mut merged: HashMap<(Source, String), Session> =
        HashMap::with_capacity(parts.size_hint().0);
    for mut part in parts {
        let key = (part.source, part.id.clone());
        match merged.get_mut(&key) {
            None => {
                merged.insert(key, part);
            }
            Some(existing) => {
                if part.source == Source::VsCode {
                    let candidate = part.vscode.as_ref();
                    let current = existing.vscode.as_ref();
                    let candidate_key = (
                        candidate
                            .map_or(vscode::StorageFormat::Transcript, |context| context.format),
                        part.updated,
                        candidate.and_then(|context| context.target.as_ref()),
                        candidate.map(|context| context.editor.command()),
                    );
                    let current_key = (
                        current.map_or(vscode::StorageFormat::Transcript, |context| context.format),
                        existing.updated,
                        current.and_then(|context| context.target.as_ref()),
                        current.map(|context| context.editor.command()),
                    );
                    if candidate_key > current_key {
                        *existing = part;
                    }
                    continue;
                }
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
                if !part.assistant_text.is_empty()
                    && !existing.assistant_text.contains(&part.assistant_text)
                {
                    if !existing.assistant_text.is_empty() {
                        existing.assistant_text.push('\n');
                    }
                    existing.assistant_text.push_str(&part.assistant_text);
                }
            }
        }
    }
    let mut sessions: Vec<_> = merged
        .into_values()
        .filter(|session| {
            session.source != Source::VsCode
                || !session.search_text.is_empty()
                || !session.assistant_text.is_empty()
        })
        .collect();
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    sessions
}

fn read_cache(path: &Path) -> Cache {
    let Ok(bytes) = fs::read(path) else {
        return Cache::default();
    };
    match serde_json::from_slice::<Cache>(&bytes) {
        Ok(cache) if cache.version == CACHE_VERSION => cache,
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
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let mut file = BufWriter::new(file);
    serde_json::to_writer(&mut file, cache)?;
    file.flush()?;
    file.get_ref().sync_all()?;
    fs::rename(tmp, path)
}

fn gui_command(session: &Session) -> Result<Command, &'static str> {
    if session.source == Source::Copilot {
        return Err("Copilot CLI has no supported desktop session link; use Enter or --print to copy its resume command");
    }
    if session.source == Source::VsCode {
        return vscode_open::command(session, true);
    }
    if !codex::is_uuid(&session.id) {
        return Err("session has an invalid ID");
    }
    let mut command = match session.source {
        Source::Claude => Command::new("claude"),
        Source::Codex | Source::Cursor => Command::new("open"),
        Source::Copilot | Source::VsCode => unreachable!(),
    };
    match session.source {
        Source::Claude => {
            command
                .current_dir(&session.cwd)
                .args(["--desktop", "--resume", &session.id]);
        }
        Source::Codex => {
            command.arg(format!("codex://threads/{}", session.id));
        }
        Source::Cursor => {
            command.arg(cursor_url(session));
        }
        Source::Copilot | Source::VsCode => unreachable!(),
    }
    Ok(command)
}

fn open_gui(session: &Session) -> Result<(), Box<dyn std::error::Error>> {
    warn_if_missing(session);
    if session.source == Source::VsCode {
        vscode_open::open(session)?;
        println!(
            "Requested VS Code session {} in its recorded workspace.",
            session.id
        );
        return Ok(());
    }
    let mut command = gui_command(session)?;
    if session.source == Source::Claude && !io::stdout().is_terminal() {
        // Claude's desktop handoff rejects redirected output, including CLI wrappers.
        let terminal = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| "Claude Desktop opening requires a terminal")?;
        command.stdout(terminal.try_clone()?).stderr(terminal);
    }
    let status = command.status()?;
    if !status.success() {
        return Err(format!("could not open {}: {status}", source_name(session.source)).into());
    }
    println!(
        "Requested {} session {} in its app. PWD: {}",
        source_name(session.source),
        session.id,
        session.cwd.display()
    );
    Ok(())
}

fn cursor_url(session: &Session) -> String {
    const PREFIX: &str = "cursor://session-finder.cursor-session-opener/open?id=";
    let cwd = session.cwd.as_os_str().as_encoded_bytes();
    let mut url =
        String::with_capacity(PREFIX.len() + session.id.len() + "&cwd=".len() + cwd.len());
    url.push_str(PREFIX);
    url.push_str(&session.id);
    url.push_str("&cwd=");
    push_url_encoded(&mut url, cwd);
    url
}

fn push_url_encoded(url: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push('%');
            url.push(char::from(HEX[usize::from(byte >> 4)]));
            url.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::Claude => "Claude",
        Source::Codex => "Codex",
        Source::Copilot => "Copilot",
        Source::Cursor => "Cursor",
        Source::VsCode => "VSCode",
    }
}

fn display_source(session: &Session) -> &'static str {
    match (session.source, session.archived) {
        (Source::Claude, true) => "Claude*",
        (Source::Codex, true) => "Codex*",
        (Source::Cursor, true) => "Cursor*",
        (Source::Copilot, true) => "Copilot*",
        (Source::VsCode, true) => "VSCode*",
        (source, false) => source_name(source),
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
        Source::Copilot => ["cd -- ", &cwd, " && copilot --resume=", &id].concat(),
        Source::Cursor => ["open ", &shell_quote(&cursor_url(session))].concat(),
        Source::VsCode if vscode_open::command(session, true).is_ok() => {
            let executable = std::env::current_exe().unwrap_or_else(|_| "session-finder".into());
            [
                &shell_quote(&executable.to_string_lossy()),
                " --open ",
                &shell_quote(&picker::identity(session)),
            ]
            .concat()
        }
        Source::VsCode
            if session.cwd.as_os_str().is_empty()
                && session
                    .vscode
                    .as_ref()
                    .and_then(|context| context.target.as_ref())
                    .is_none() =>
        {
            format!(
                "# VS Code chat {}: reopen it from VS Code's Chat sessions view",
                session.id
            )
        }
        Source::VsCode => {
            let context = session.vscode.as_ref();
            let editor = context.map_or("code", |context| context.editor.command());
            let target = context
                .and_then(|context| context.target.as_deref())
                .unwrap_or(&session.cwd);
            [editor, " ", &shell_quote(&target.to_string_lossy())].concat()
        }
    }
}

fn warn_if_missing(session: &Session) {
    if session.source == Source::VsCode {
        if let Some(target) = session
            .vscode
            .as_ref()
            .and_then(|context| context.target.as_deref())
        {
            if !target.exists() {
                eprintln!("Recorded workspace is missing: {}", target.display());
            }
            return;
        }
        if session.cwd.as_os_str().is_empty() {
            return;
        }
    }
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
    fn migrated_vscode_copies_keep_the_known_workspace_in_either_order() {
        let empty = Session {
            source: Source::VsCode,
            id: "11111111-2222-4333-8444-555555555555".into(),
            cwd: PathBuf::new(),
            updated: 123,
            search_text: "request".into(),
            assistant_text: String::new(),
            preview: "request".into(),
            archived: false,
            vscode: Some(vscode::Context {
                format: vscode::StorageFormat::Journal,
                ..Default::default()
            }),
        };
        let mut attached = empty.clone();
        attached.cwd = "/work".into();
        attached.vscode.as_mut().unwrap().target = Some("/work".into());
        for parts in [vec![empty.clone(), attached.clone()], vec![attached, empty]] {
            let merged = merge_sessions(parts);
            assert_eq!(merged.len(), 1);
            assert_eq!(resume_command(&merged[0]), "code '/work'");
        }
    }

    #[test]
    fn resume_command_quotes_untrusted_cwd() {
        let session = Session {
            source: Source::Claude,
            id: "abc".into(),
            cwd: PathBuf::from("/tmp/O'Brien; echo bad"),
            updated: 0,
            search_text: String::new(),
            assistant_text: String::new(),
            preview: String::new(),
            archived: false,
            vscode: None,
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
                assistant_text: "earlier answer".into(),
                preview: "earlier".into(),
                archived: false,
                vscode: None,
            },
            Session {
                source: Source::Codex,
                id: "same".into(),
                cwd: "/new".into(),
                updated: 20,
                search_text: "later words".into(),
                assistant_text: "later answer".into(),
                preview: "later".into(),
                archived: false,
                vscode: None,
            },
        ];
        let merged = merge_sessions(parts);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].cwd, PathBuf::from("/new"));
        assert!(merged[0].search_text.contains("earlier words"));
        assert!(merged[0].search_text.contains("later words"));
        assert!(merged[0].assistant_text.contains("earlier answer"));
        assert!(merged[0].assistant_text.contains("later answer"));
    }

    #[test]
    fn archived_codex_resumes_by_id() {
        let session = Session {
            source: Source::Codex,
            id: "abc".into(),
            cwd: "/work".into(),
            updated: 1,
            search_text: String::new(),
            assistant_text: String::new(),
            preview: String::new(),
            archived: true,
            vscode: None,
        };
        assert_eq!(resume_command(&session), "codex resume 'abc' -C '/work'");
    }

    #[test]
    fn gui_commands_target_exact_sessions_without_a_shell() {
        let mut session = Session {
            source: Source::Codex,
            id: "019c675d-0e41-7f71-925f-1ccc0da0f692".into(),
            cwd: "/work".into(),
            updated: 0,
            search_text: String::new(),
            assistant_text: String::new(),
            preview: String::new(),
            archived: false,
            vscode: None,
        };
        let command = gui_command(&session).unwrap();
        assert_eq!(command.get_program(), "open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["codex://threads/019c675d-0e41-7f71-925f-1ccc0da0f692"]
        );
        session.source = Source::Claude;
        session.cwd = "/tmp/O'Brien; echo bad".into();
        let command = gui_command(&session).unwrap();
        assert_eq!(command.get_program(), "claude");
        assert_eq!(command.get_current_dir(), Some(session.cwd.as_path()));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--desktop", "--resume", &session.id]
        );
        session.source = Source::Cursor;
        let command = gui_command(&session).unwrap();
        assert_eq!(command.get_program(), "open");
        let url = cursor_url(&session);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new(&url)]
        );
        assert_eq!(resume_command(&session), "open 'cursor://session-finder.cursor-session-opener/open?id=019c675d-0e41-7f71-925f-1ccc0da0f692&cwd=%2Ftmp%2FO%27Brien%3B%20echo%20bad'");
        session.cwd = "/tmp/é?&#".into();
        assert!(cursor_url(&session).ends_with("cwd=%2Ftmp%2F%C3%A9%3F%26%23"));
        session.id = "../new".into();
        assert!(gui_command(&session).is_err());
    }

    #[test]
    fn cache_writes_round_trip_and_replace_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("session-cache-roundtrip-{}", std::process::id()));
        let path = root.join("index.json");
        let mut cache = Cache {
            version: CACHE_VERSION,
            entries: HashMap::from([(
                "/fixture.jsonl".into(),
                CacheEntry {
                    modified_ns: 1_234_567_890_123_456_789,
                    len: 42,
                    session: Some(Session {
                        source: Source::Claude,
                        id: "session-id".into(),
                        cwd: "/tmp/α\\work".into(),
                        updated: 123,
                        search_text: "quoted \"message\"\nnext line".into(),
                        assistant_text: "answer\tβ".into(),
                        preview: "quoted \"message\"".into(),
                        archived: true,
                        vscode: None,
                    }),
                },
            )]),
        };
        write_cache(&path, &cache).unwrap();
        let loaded = read_cache(&path);
        assert_eq!(loaded.version, CACHE_VERSION);
        let entry = &loaded.entries["/fixture.jsonl"];
        assert_eq!(entry.modified_ns, 1_234_567_890_123_456_789);
        assert_eq!(entry.len, 42);
        let session = entry.session.as_ref().unwrap();
        assert_eq!(session.cwd, Path::new("/tmp/α\\work"));
        assert_eq!(session.search_text, "quoted \"message\"\nnext line");
        assert_eq!(session.assistant_text, "answer\tβ");
        assert!(session.archived);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );

        cache.entries.clear();
        write_cache(&path, &cache).unwrap();
        assert!(read_cache(&path).entries.is_empty());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_old_cache_after_parser_policy_changes() {
        let path =
            std::env::temp_dir().join(format!("session-cache-version-{}.json", std::process::id()));
        fs::write(&path, r#"{"version":4,"entries":{}}"#).unwrap();
        assert_eq!(read_cache(&path).version, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn optional_strings_borrow_unescaped_json() {
        #[derive(Deserialize)]
        struct Field<'a> {
            #[serde(borrow)]
            value: Option<BorrowedStr<'a>>,
        }

        let plain: Field<'_> = serde_json::from_str(r#"{"value":"plain"}"#).unwrap();
        assert!(matches!(
            plain.value,
            Some(BorrowedStr(Cow::Borrowed("plain")))
        ));
        let escaped: Field<'_> = serde_json::from_str(r#"{"value":"a\"b"}"#).unwrap();
        assert!(matches!(escaped.value, Some(BorrowedStr(Cow::Owned(value))) if value == "a\"b"));
        let missing: Field<'_> = serde_json::from_str("{}").unwrap();
        assert!(missing.value.is_none());
        let direct: BorrowedStr<'_> = serde_json::from_str(r#""plain""#).unwrap();
        assert!(matches!(direct, BorrowedStr(Cow::Borrowed("plain"))));
    }
}
