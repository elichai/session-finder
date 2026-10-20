use crate::{codex::is_uuid, copilot, session_files::SessionFiles, Session, Source};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    io::{self, BufReader},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Default VS Code `workspaceStorage` directories, relative to `$HOME`.
pub const DEFAULT_ROOTS: [&str; 6] = [
    ".config/Code/User/workspaceStorage",
    ".config/Code - Insiders/User/workspaceStorage",
    "Library/Application Support/Code/User/workspaceStorage",
    "Library/Application Support/Code - Insiders/User/workspaceStorage",
    ".vscode-server/data/User/workspaceStorage",
    ".vscode-server-insiders/data/User/workspaceStorage",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Editor {
    #[default]
    Code,
    Insiders,
}

impl Editor {
    pub fn command(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Insiders => "code-insiders",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Context {
    pub editor: Editor,
    pub target: Option<PathBuf>,
    pub title: String,
    pub participants: Vec<String>,
    pub format: StorageFormat,
    #[serde(skip)]
    pub storage: Option<PathBuf>,
    /// Server-side storage of a Remote-SSH chat, used to recover its folder on demand.
    #[serde(skip)]
    pub remote_storage: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum StorageFormat {
    #[default]
    Transcript,
    Snapshot,
    Journal,
}

#[derive(Deserialize)]
struct Workspace {
    folder: Option<String>,
    workspace: Option<String>,
}

/// Scan saved chat sessions and supplemental Copilot hook transcripts only.
pub fn session_files(root: &Path) -> SessionFiles {
    let directories = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .flat_map(|entry| {
            [
                entry.path().join("chatSessions"),
                entry.path().join("GitHub.copilot-chat/transcripts"),
            ]
        });
    let empty_windows = root
        .parent()
        .map(|user| user.join("globalStorage/emptyWindowChatSessions"));
    SessionFiles::from_dirs(directories.chain(empty_windows), |path| {
        path.extension()
            .is_some_and(|extension| extension == "jsonl" || extension == "json")
            && path.parent().and_then(Path::file_name).is_some_and(|name| {
                name == "transcripts" || name == "chatSessions" || name == "emptyWindowChatSessions"
            })
            && session_id(path).is_some()
    })
}

fn session_id(path: &Path) -> Option<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| is_uuid(id))
}

pub fn parse_file(path: &Path) -> io::Result<Option<Session>> {
    if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "chatSessions" || name == "emptyWindowChatSessions")
    {
        return crate::vscode_native::parse_file(path);
    }
    let Some(id) = session_id(path) else {
        return Ok(None);
    };
    let modified = fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs() as i64;
    copilot::parse_events(
        Source::VsCode,
        id,
        BufReader::new(fs::File::open(path)?),
        modified,
    )
}

/// Resolve the sidecar after loading cached text so sidecar changes take effect immediately.
pub fn resolve_context(path: &Path, session: &mut Session) {
    let storage = if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "chatSessions")
    {
        path.ancestors().nth(2)
    } else if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "transcripts")
    {
        path.ancestors().nth(3)
    } else {
        None
    };
    let context = session.vscode.get_or_insert_with(Context::default);
    context.editor = if path.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some("Code - Insiders" | ".vscode-server-insiders")
        )
    }) {
        Editor::Insiders
    } else {
        Editor::Code
    };
    let workspace = storage
        .and_then(|storage| fs::read(storage.join("workspace.json")).ok())
        .and_then(|text| serde_json::from_slice::<Workspace>(&text).ok());
    let folder = workspace
        .as_ref()
        .and_then(|workspace| workspace.folder.as_deref())
        .and_then(file_uri_path);
    context.target = workspace
        .as_ref()
        .and_then(|workspace| workspace.workspace.as_deref())
        .and_then(file_uri_path)
        .or_else(|| folder.clone());
    // Canonicalize so a symlinked desktop path (e.g. `~/.config/Code/User`) is still recognized.
    context.remote_storage = storage
        .and_then(|storage| fs::canonicalize(storage).ok())
        .filter(|storage| {
            storage.components().any(|part| {
                matches!(
                    part.as_os_str().to_str(),
                    Some(".vscode-server" | ".vscode-server-insiders")
                )
            })
        });
    // A working-directory fallback does not identify the store owning a native chat.
    context.storage = storage
        .filter(|_| context.target.is_some())
        .map(Path::to_path_buf);
    if session.cwd.as_os_str().is_empty() {
        session.cwd = folder.unwrap_or_default();
    }
    if context.target.is_none() && !session.cwd.as_os_str().is_empty() {
        context.target = Some(session.cwd.clone());
    }
}

pub fn details(session: &Session) -> String {
    let Some(context) = session.vscode.as_ref() else {
        return String::new();
    };
    let mut details = match context.editor {
        Editor::Code => String::from("VS Code"),
        Editor::Insiders => String::from("VS Code Insiders"),
    };
    if let Some(target) = &context.target {
        details.push_str(" • Workspace: ");
        details.push_str(&target.to_string_lossy());
    }
    if !context.title.is_empty() {
        details.push_str(" • Title: ");
        details.push_str(&context.title);
    }
    if !context.participants.is_empty() {
        details.push_str(" • Participants: ");
        details.push_str(&context.participants.join(", "));
    }
    details
}

pub(crate) fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?.as_bytes();
    if encoded.first() != Some(&b'/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    if bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_local_file_uris_only() {
        assert_eq!(
            file_uri_path("file:///home/me/My%20Project/%C3%A9").unwrap(),
            Path::new("/home/me/My Project/é")
        );
        for uri in [
            "vscode-remote://ssh-remote%2Bhost/home/me",
            "file://server/share",
            "file:///bad%2",
            "file:///bad%zz",
            "file:///nul%00",
        ] {
            assert!(file_uri_path(uri).is_none(), "{uri}");
        }
    }
}
