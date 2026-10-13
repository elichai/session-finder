# session-finder

Find a local Claude Code, Codex, or Cursor conversation by its working directory or by text you wrote in it. The panel starts with the most recently used session. Search matches the typed phrase; run `session-finder --fuzzy` for looser subsequence matching.

`fzf` still reads `FZF_DEFAULT_OPTS` for your preferred appearance and key bindings. The tool sets its search mode explicitly.

## Install

Requires a current stable [Rust toolchain](https://rust-lang.org/tools/install/), system SQLite, and **fzf 0.67.0 or later**. The picker uses `--freeze-left`, added in [fzf 0.67.0](https://github.com/junegunn/fzf/releases/tag/v0.67.0). Install the Claude/Codex CLI to resume those providers in a terminal.

### macOS (Homebrew)

```sh
brew install fzf pkgconf sqlite
```

Rust also needs Apple's command line developer tools; run `xcode-select --install` if they are missing.

### Linux (Debian / Ubuntu)

```sh
sudo apt-get update
sudo apt-get install fzf build-essential pkg-config libsqlite3-dev
fzf --version
```

Distribution packages can be older than 0.67.0. If yours is, install a recent binary from the [official fzf releases](https://github.com/junegunn/fzf/releases) and put it ahead of the older binary in `PATH`.

Linux supports the Claude/Codex history panel and printed terminal resume commands. Clipboard copying and Codex/Cursor GUI opening currently use macOS tools (`pbcopy` and `open`). The Cursor helper installation instructions below also target macOS. Cursor's default database path is specific to macOS; use `SESSION_FINDER_CURSOR_DB` to point at its database on Linux.

### Install Session Finder

From the repository directory:

```sh
cargo install --path .
session-finder
```

On macOS, press Enter to copy and print the resume command, or Ctrl-O to open the selected chat in its app. If clipboard copying is unavailable, the command is still printed. Press Esc to close the panel. The detail pane shows the full working directory and session ID. A `*` marks an archived chat.

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

## Index and automatic updates

The Claude/Codex index is saved at **`~/.cache/session-finder/index.json`** on both macOS and Linux. It stores each source file's size and modification time, plus the session ID, working directory, activity time, preview, and searchable user messages. It is a local JSON cache, not a background service.

The first run reads Claude history from `~/.claude/projects`, Codex history from `~/.codex/sessions`, and archived Codex history from `~/.codex/archived_sessions`. Internal Codex review and subagent runs are excluded. Each later launch reuses unchanged entries, reparses new or changed files, and drops deleted files automatically. **No manual rebuild is needed for normal use.** `--rebuild` forces a full reparse; a missing, invalid, or incompatible cache is rebuilt automatically.

Cursor's desktop SQLite database is read on every launch, using a consistent read-only snapshot that includes committed WAL updates. Session metadata supplies the exact workspace or worktree path and latest activity time. Subagent chats and empty drafts are excluded. Missing Cursor history is fine; a database error is reported while the other providers remain searchable.

Cursor sessions are not written to the JSON cache. An open panel keeps its launch-time snapshot; reopen it to see new messages. Search includes user messages, session IDs, and full working directories; Cursor chat titles are also searchable.

The cache contains your messages in plain text. New cache directories are created with mode `0700`, and the cache file with mode `0600`. Cache updates use a temporary file and rename it into place. Session Finder does not upload history or the cache.

To change the cache location:

```sh
SESSION_FINDER_CACHE=/path/to/private/index.json session-finder
```

`SESSION_FINDER_CLAUDE_ROOT`, `SESSION_FINDER_CODEX_ROOT`, and `SESSION_FINDER_CODEX_ARCHIVE_ROOT` override history locations. `SESSION_FINDER_CURSOR_DB` overrides the Cursor database path, which defaults to `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`.

## Tests

Run parser tests with `cargo test`, and helper tests with `node --test cursor-extension/extension.test.js`.
