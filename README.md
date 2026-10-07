# session-finder

Find a local Claude Code, Codex, or Cursor conversation by its working directory or by text you wrote in it. The panel starts with the most recently used session. Search matches the typed phrase; run `session-finder --fuzzy` for looser subsequence matching.

`fzf` still reads `FZF_DEFAULT_OPTS` for your preferred appearance and key bindings. The tool sets its search mode explicitly.

## Install

Requires Rust, system SQLite, and `fzf`. Install the Claude/Codex CLI to resume those providers in a terminal. On macOS, the selected resume command is copied with `pbcopy`.

```sh
cargo install --path .
session-finder
```

Press Enter to copy and print the resume command, or Ctrl-O to open the selected chat in its app. Press Esc to close the panel. The detail pane shows the full working directory and session ID. A `*` marks an archived chat.

For Claude, Ctrl-O uses `claude --desktop --resume ID` in the recorded directory. This requires Claude Code 2.1.285 or later, the Claude Desktop app, and a Claude subscription. Claude rejects a session that is still running in another terminal. Codex uses its desktop session link; terminal resume restores an archived Codex chat to active history.

For Cursor, both Enter's copied command and Ctrl-O use the desktop app, through the small local Session Finder helper extension. They target the existing chat ID; they do not start a CLI conversation. Install the helper once from the repository:

```sh
cd cursor-extension
zip /tmp/session-finder-cursor.vsix extension.vsixmanifest '[Content_Types].xml' extension/package.json extension/extension.js
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --install-extension /tmp/session-finder-cursor.vsix
```

After updating the helper, reload the Cursor window. The helper opens the recorded workspace or worktree before selecting the exact chat. It uses Cursor's internal existing-session commands, which may change with Cursor updates. In classic mode, Cursor may show the saved transcript if the chat is no longer available. The helper reports errors in the app and logs actions to the “Session Finder” output channel.

## Other commands

```sh
session-finder --list              # recent-first list
session-finder --print SESSION_ID  # print one resume command
session-finder --open SESSION_ID   # open that session in its app
session-finder --rebuild           # reindex every session file
session-finder --fuzzy             # looser matching
```

The first run reads Claude and Codex session files, including archived Codex sessions. Internal Codex review and subagent runs are excluded. Each later launch checks file size and modification time, reparses changed files, and drops deleted files automatically. `--rebuild` forces a full reparse.

Cursor's desktop SQLite database is read on every launch, using a consistent read-only snapshot that includes committed WAL updates. Session metadata supplies the exact workspace or worktree path and latest activity time. Subagent chats and empty drafts are excluded. Missing Cursor history is fine; a database error is reported while the other providers remain searchable.

An open panel keeps its launch-time snapshot; reopen it to see new messages. Search includes user messages, session IDs, and full working directories; Cursor chat titles are also searchable. The Claude/Codex cache is a private local JSON file at `~/.cache/session-finder/index.json`; it contains your messages in plain text. Set `SESSION_FINDER_CACHE` to use another path. `SESSION_FINDER_CLAUDE_ROOT`, `SESSION_FINDER_CODEX_ROOT`, and `SESSION_FINDER_CODEX_ARCHIVE_ROOT` override history locations. `SESSION_FINDER_CURSOR_DB` overrides the Cursor database path, which defaults to `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`.

Run parser tests with `cargo test`, and helper tests with `node --test cursor-extension/extension.test.js`.
