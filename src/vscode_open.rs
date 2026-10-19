use crate::{codex::is_uuid, push_url_encoded, vscode, Session, Source};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Target<'a> {
    session: &'a Session,
    workspace: &'a Path,
    storage: &'a Path,
    user_data: &'a Path,
    editor: vscode::Editor,
}

impl<'a> Target<'a> {
    fn new(session: &'a Session) -> Result<Self, &'static str> {
        if session.source != Source::VsCode || !is_uuid(&session.id) {
            return Err("VS Code chat has an invalid session ID");
        }
        let context = session
            .vscode
            .as_ref()
            .ok_or("VS Code chat has no workspace metadata")?;
        if context.format == vscode::StorageFormat::Transcript {
            return Err(
                "VS Code chat has only a recovery transcript; no saved native session is available",
            );
        }
        let storage = context
            .storage
            .as_deref()
            .ok_or("VS Code chat has no recorded local workspace")?;
        if storage.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some(".vscode-server" | ".vscode-server-insiders")
            )
        }) {
            return Err("Remote VS Code history cannot be opened by a local editor");
        }
        let user = storage
            .parent()
            .filter(|root| {
                root.file_name()
                    .is_some_and(|name| name == "workspaceStorage")
            })
            .and_then(Path::parent)
            .filter(|root| root.file_name().is_some_and(|name| name == "User"));
        let user_data = user
            .and_then(Path::parent)
            .ok_or("VS Code chat has an unrecognized user-data location")?;
        let workspace = context
            .target
            .as_deref()
            .ok_or("VS Code chat has no recorded workspace")?;
        for path in [workspace, user_data] {
            let text = path
                .to_str()
                .ok_or("VS Code opening requires UTF-8 paths")?;
            if !path.is_absolute() || text.chars().any(char::is_control) {
                return Err("VS Code opening requires absolute paths without control characters");
            }
        }
        Ok(Self {
            session,
            workspace,
            storage,
            user_data,
            editor: context.editor,
        })
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(self.editor.command());
        command.arg("--user-data-dir").arg(self.user_data);
        command
    }

    fn url(&self, new_window: bool) -> String {
        let mut url = match self.editor {
            vscode::Editor::Code => "vscode://file",
            vscode::Editor::Insiders => "vscode-insiders://file",
        }
        .to_owned();
        for (index, component) in self.workspace.to_str().unwrap().split('/').enumerate() {
            if index > 0 {
                url.push('/');
            }
            push_url_encoded(&mut url, component.as_bytes());
        }
        url.push_str(if new_window {
            "?windowId=_blank&session="
        } else {
            "?session="
        });
        let mut resource = String::from("vscode-chat-session://local/");
        const BASE64: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        // UUID text is 36 ASCII bytes, so base64 has no partial block or padding.
        for block in self.session.id.as_bytes().as_chunks::<3>().0 {
            let value = u32::from(block[0]) << 16 | u32::from(block[1]) << 8 | u32::from(block[2]);
            for shift in [18, 12, 6, 0] {
                resource.push(char::from(BASE64[((value >> shift) & 63) as usize]));
            }
        }
        push_url_encoded(&mut url, resource.as_bytes());
        url
    }

    fn command(&self, new_window: bool) -> Command {
        let mut command = self.cli();
        command.arg("--open-url").arg(self.url(new_window));
        command
    }

    fn check_saved(&self) -> io::Result<()> {
        if !self.workspace.is_dir()
            && !(self
                .workspace
                .extension()
                .is_some_and(|ext| ext == "code-workspace")
                && self.workspace.is_file())
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "recorded VS Code workspace is missing",
            ));
        }
        if ![".jsonl", ".json"].iter().any(|suffix| {
            self.storage
                .join("chatSessions")
                .join([self.session.id.as_str(), suffix].concat())
                .is_file()
        }) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "saved VS Code chat is no longer available",
            ));
        }
        Ok(())
    }

    fn ready(&self, deadline: Instant) -> io::Result<bool> {
        let mut command = self.cli();
        command.arg("--status");
        let output = run(
            command,
            deadline.min(Instant::now() + Duration::from_secs(5)),
        )?;
        let text = String::from_utf8_lossy(&output);
        // Code exits successfully even when no instance is running.
        Ok(text.lines().any(|line| line.starts_with("Version:"))
            && text.lines().any(|line| line.starts_with("Process Argv:")))
    }
}

pub fn command(session: &Session, new_window: bool) -> Result<Command, &'static str> {
    Ok(Target::new(session)?.command(new_window))
}

pub fn open(session: &Session) -> Result<(), Box<dyn std::error::Error>> {
    let target = Target::new(session)?;
    target.check_saved()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let running = target.ready(deadline)?;
    if !running {
        let mut launch = target.cli();
        launch.arg("--new-window").arg(target.workspace);
        run(launch, deadline)?;
        while !target.ready(deadline)? {
            if Instant::now() >= deadline {
                return Err("VS Code did not become ready to open the chat".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    target.check_saved()?;
    // Initial protocol URLs lose `session`; only send it after the runtime handler is ready.
    // A cold launch already opened the target, so reuse it instead of creating a duplicate.
    run(target.command(running), deadline)?;
    Ok(())
}

fn run(mut command: Command, deadline: Instant) -> io::Result<Vec<u8>> {
    static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(
        [
            "session-finder-code-",
            &std::process::id().to_string(),
            "-",
            &SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
                .to_string(),
            "-",
            &NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed).to_string(),
        ]
        .concat(),
    );
    let mut capture = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    fs::remove_file(path)?;
    // A regular file avoids pipe backpressure and EOF waits on inherited stdout.
    let mut child = command
        .stdin(Stdio::null())
        .stdout(capture.try_clone()?)
        .stderr(Stdio::null())
        .spawn()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(io::Error::other(
                        ["VS Code command failed: ", &status.to_string()].concat(),
                    ));
                }
                capture.seek(SeekFrom::Start(0))?;
                let mut output = Vec::new();
                capture.take(1024 * 1024).read_to_end(&mut output)?;
                return Ok(output);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result.err().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "VS Code command timed out")
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn script(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(
            [
                "session-finder-open-",
                &std::process::id().to_string(),
                "-",
                name,
            ]
            .concat(),
        );
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn reads_output_larger_than_a_pipe_buffer() {
        let path = script(
            "large",
            "/bin/dd if=/dev/zero bs=1024 count=512 2>/dev/null\n",
        );
        let mut command = Command::new("/bin/sh");
        command.arg(&path);
        let output = run(command, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(output.len(), 512 * 1024);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn does_not_wait_for_descendants_to_close_stdout() {
        let path = script("descendant", "/bin/sleep 2 &\nprintf 'ready'\n");
        let mut command = Command::new("/bin/sh");
        command.arg(&path);
        let start = Instant::now();
        let output = run(command, start + Duration::from_millis(200)).unwrap();
        assert_eq!(output, b"ready");
        assert!(start.elapsed() < Duration::from_millis(800));
        fs::remove_file(path).unwrap();
    }
}
