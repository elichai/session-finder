use crate::{codex::is_uuid, copilot, session_files::SessionFiles, Session, Source};
use serde::Deserialize;
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

#[derive(Deserialize)]
struct Workspace {
    folder: Option<String>,
    workspace: Option<String>,
}

/// Lists `<root>/<workspace>/GitHub.copilot-chat/transcripts/<uuid>.jsonl` without
/// walking the rest of workspaceStorage, which can hold gigabytes of unrelated data.
pub fn session_files(root: &Path) -> SessionFiles {
    let transcripts = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path().join("GitHub.copilot-chat/transcripts"));
    SessionFiles::from_dirs(transcripts, |path| {
        path.extension()
            .is_some_and(|extension| extension == "jsonl")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "transcripts")
            && session_id(path).is_some()
    })
}

fn session_id(path: &Path) -> Option<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| is_uuid(id))
}

pub fn parse_file(path: &Path) -> io::Result<Option<Session>> {
    let Some(id) = session_id(path) else {
        return Ok(None);
    };
    let modified = fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs() as i64;
    let session = copilot::parse_events(
        Source::VsCode,
        id,
        BufReader::new(fs::File::open(path)?),
        modified,
    )?;
    Ok(session.map(|mut session| {
        // Remote (SSH/WSL/container) workspaces have no workspace.json on the server.
        if session.cwd.as_os_str().is_empty() {
            session.cwd = path
                .ancestors()
                .nth(3)
                .and_then(workspace_folder)
                .unwrap_or_default();
        }
        session
    }))
}

fn workspace_folder(storage: &Path) -> Option<PathBuf> {
    let text = fs::read(storage.join("workspace.json")).ok()?;
    let workspace: Workspace = serde_json::from_slice(&text).ok()?;
    if let Some(folder) = workspace.folder {
        return file_uri_path(&folder);
    }
    file_uri_path(&workspace.workspace?)?
        .parent()
        .map(Path::to_path_buf)
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
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

    #[test]
    fn reads_folder_or_multi_root_workspace_location() {
        let root = std::env::temp_dir().join(format!("session-vscode-ws-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"folder":"file:///work/app"}"#,
        )
        .unwrap();
        assert_eq!(workspace_folder(&root).unwrap(), Path::new("/work/app"));
        fs::write(
            root.join("workspace.json"),
            r#"{"workspace":"file:///work/all.code-workspace"}"#,
        )
        .unwrap();
        assert_eq!(workspace_folder(&root).unwrap(), Path::new("/work"));
        fs::remove_file(root.join("workspace.json")).unwrap();
        assert!(workspace_folder(&root).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
