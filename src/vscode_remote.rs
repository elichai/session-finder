//! Reopen Remote-SSH chats whose history lives under `~/.vscode-server`.
//!
//! Server storage has no `workspace.json`; its directory name is
//! `md5("vscode-remote://ssh-remote%2B<host><folder>")`. Copilot's debug log records the
//! workspace folder, so a candidate is trusted only when it reproduces that hash.
use crate::{codex::is_uuid, push_url_encoded, vscode, vscode_open, Session};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Component, Path},
};

const MARKER: &str = "workspace with the following folders:";
const SCAN_BYTES: u64 = 1024 * 1024;
const MAX_LOGS: usize = 32;

/// `vscode://vscode-remote/ssh-remote+HOST/FOLDER?session=...` for a verified server chat.
pub fn url(session: &Session) -> Option<String> {
    let context = session.vscode.as_ref()?;
    let storage = context.remote_storage.as_deref()?;
    let name = storage.file_name()?.to_str()?.to_ascii_lowercase();
    // VS Code appends `-N` when a storage directory name is already taken.
    let hash = name
        .split_once('-')
        .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map_or(name.as_str(), |(hash, _)| hash);
    if !is_uuid(&session.id) || hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hosts = hosts();
    let mut seen = HashSet::new();
    let mut matches = |folder: &str| {
        if !seen.insert(folder.to_owned()) {
            return None;
        }
        let path = encode_path(folder)?;
        hosts
            .iter()
            .find(|host| {
                hex(&md5(["vscode-remote://ssh-remote%2B", host, &path]
                    .concat()
                    .as_bytes()))
                    == hash
            })
            .map(|host| [host.as_str(), &path].concat())
    };
    let chat = storage.join("GitHub.copilot-chat");
    // This chat's own files first, then its workspace siblings (same folder by construction).
    let ordered = |dir: &str, own: String| {
        let dir = chat.join(dir);
        let first = dir.join(&own);
        let others = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(move |path| path.file_name().is_none_or(|name| name != own.as_str()));
        std::iter::once(first).chain(others).take(MAX_LOGS)
    };
    let cwd = session.cwd.to_str().filter(|cwd| !cwd.is_empty());
    let target = cwd
        .and_then(&mut matches)
        .or_else(|| {
            ordered("debug-logs", session.id.clone())
                .flat_map(|dir| logged_folders(&dir.join("main.jsonl")))
                .find_map(|folder| matches(&folder))
        })
        .or_else(|| {
            // Older chats lack the prompt; tool calls still mention files inside the folder.
            ordered("transcripts", [session.id.as_str(), ".jsonl"].concat())
                .flat_map(|file| mentioned_folders(&file))
                .find_map(|folder| matches(&folder))
        })?;
    let scheme = match context.editor {
        vscode::Editor::Code => "vscode",
        vscode::Editor::Insiders => "vscode-insiders",
    };
    let mut url = [scheme, "://vscode-remote/ssh-remote+", &target, "?session="].concat();
    push_url_encoded(&mut url, vscode_open::chat_resource(&session.id).as_bytes());
    Some(url)
}

/// Remote-SSH authorities usually match this server's full or short host name.
fn hosts() -> Vec<String> {
    let name = fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default();
    let name = name.trim();
    let mut hosts = Vec::new();
    for host in [name, name.split('.').next().unwrap_or_default()] {
        if !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
            && !hosts.iter().any(|known| known == host)
        {
            hosts.push(host.to_owned());
        }
    }
    hosts
}

fn read_prefix(path: &Path) -> String {
    let mut bytes = Vec::new();
    let _ = fs::File::open(path).and_then(|file| file.take(SCAN_BYTES).read_to_end(&mut bytes));
    String::from_utf8_lossy(&bytes).into_owned()
}

/// First folder of each "workspace with the following folders" prompt near the log start.
fn logged_folders(path: &Path) -> Vec<String> {
    let text = read_prefix(path);
    let mut folders: Vec<String> = Vec::new();
    for (index, _) in text.match_indices(MARKER) {
        // The prompt is JSON-escaped once or twice, so its newline is `\n` or `\\n`.
        let rest = text[index + MARKER.len()..].trim_start_matches('\\');
        let Some(rest) = rest.strip_prefix("n- ") else {
            continue;
        };
        let folder = rest.split('\\').next().unwrap_or_default().trim();
        if folder.starts_with('/') && !folder.contains('"') && !folders.iter().any(|f| f == folder)
        {
            folders.push(folder.to_owned());
        }
    }
    folders
}

/// Ancestors of quoted absolute paths in a transcript, deepest first.
fn mentioned_folders(path: &Path) -> Vec<String> {
    let text = read_prefix(path);
    let mut folders = Vec::new();
    let mut seen = HashSet::new();
    for (index, _) in text.match_indices("\"/") {
        let rest = &text[index + 1..];
        let end = rest
            .find(|c: char| c == '"' || c == '\\' || c == ':' || c.is_whitespace())
            .unwrap_or(rest.len());
        for ancestor in Path::new(&rest[..end]).ancestors().skip(1) {
            let Some(folder) = ancestor.to_str().filter(|f| f.len() > 1) else {
                break;
            };
            if !seen.insert(folder.to_owned()) {
                break;
            }
            folders.push(folder.to_owned());
        }
    }
    folders
}

/// Encode a path like VS Code's `URI.toString()`: unreserved bytes and `/` stay literal.
fn encode_path(folder: &str) -> Option<String> {
    let path = Path::new(folder);
    if !path.is_absolute()
        || folder.chars().any(char::is_control)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return None;
    }
    let mut encoded = String::with_capacity(folder.len());
    for (index, component) in folder.trim_end_matches('/').split('/').enumerate() {
        if index > 0 {
            encoded.push('/');
        }
        push_url_encoded(&mut encoded, component.as_bytes());
    }
    Some(encoded)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn md5(input: &[u8]) -> [u8; 16] {
    const SHIFTS: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let constants: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(input.len() as u64).wrapping_mul(8).to_le_bytes());
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for chunk in message.chunks_exact(64) {
        let words: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ])
        });
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (mix, word) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let sum = mix
                .wrapping_add(a)
                .wrapping_add(constants[i])
                .wrapping_add(words[word]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(SHIFTS[i / 16 * 4 + i % 4]));
        }
        for (value, add) in state.iter_mut().zip([a, b, c, d]) {
            *value = value.wrapping_add(add);
        }
    }
    let mut digest = [0; 16];
    for (bytes, value) in digest.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&value.to_le_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_digests_and_vscode_remote_storage() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        assert_eq!(
            hex(&md5(
                b"vscode-remote://ssh-remote%2Bepgd035/mobileye/SW_archive/amitait/trees/app_09_26_v2/app/validation/integration_tests"
            )),
            "c00a0e80c920b5ac20d8b6e604c557d0"
        );
    }

    #[test]
    fn encodes_paths_like_vscode_uris() {
        assert_eq!(encode_path("/a b/c+d/").unwrap(), "/a%20b/c%2Bd");
        assert!(encode_path("relative").is_none());
        assert!(encode_path("/a/../b").is_none());
    }

    #[test]
    fn reads_single_and_double_escaped_workspace_prompts() {
        let dir = std::env::temp_dir().join(format!("sf-remote-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("main.jsonl");
        fs::write(
            &log,
            concat!(
                r#"{"a":"I am working in a workspace with the following folders:\\n- /work/one \\nI am"}"#,
                "\n",
                r#"{"a":"workspace with the following folders:\n- /work/two\n- /work/three"}"#,
                "\n",
                r#"{"a":"workspace with the following folders:\n- relative"}"#,
            ),
        )
        .unwrap();
        assert_eq!(logged_folders(&log), ["/work/one", "/work/two"]);
        let transcript = dir.join("t.jsonl");
        fs::write(
            &transcript,
            r#"{"args":{"filePath":"/work/app/src/a.rs","b":"/work/app/src/b.rs:12"}}"#,
        )
        .unwrap();
        assert_eq!(
            mentioned_folders(&transcript),
            ["/work/app/src", "/work/app", "/work"]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
