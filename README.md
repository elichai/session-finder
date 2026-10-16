# session-finder

Find a local Claude Code, Codex, or Cursor conversation by its working directory or by text you wrote in it. The panel starts with the most recently used session. Search matches the typed phrase; run `session-finder --fuzzy` for looser subsequence matching.

`fzf` still reads `FZF_DEFAULT_OPTS` for your preferred appearance and key bindings. The tool sets its search mode explicitly. Both modes use literal characters and smart case: lowercase queries ignore case; a query containing uppercase letters is case-sensitive. Accents are distinct, and extended-search operators are disabled, including with `--fuzzy`.

## Install

Requires **fzf 0.71.0 or later**. The picker uses field-based selection tracking across refreshes, added in fzf 0.71.0. SQLite is bundled into the binary; no system SQLite installation is needed. Install the Claude/Codex CLI to resume those providers in a terminal.

### macOS (Homebrew)

```sh
brew install fzf
```

Building from source also requires a current stable [Rust toolchain](https://rust-lang.org/tools/install/) and Apple's command line developer tools; run `xcode-select --install` if they are missing.

### Linux (Debian / Ubuntu)

```sh
sudo apt-get update
sudo apt-get install fzf
fzf --version
```

Distribution packages can be older than 0.71.0. If yours is, install a recent binary from the [official fzf releases](https://github.com/junegunn/fzf/releases) and put it ahead of the older binary in `PATH`.

Linux supports the Claude/Codex history panel and printed terminal resume commands. Clipboard copying and Codex/Cursor GUI opening currently use macOS tools (`pbcopy` and `open`). The Cursor helper installation instructions below also target macOS. Cursor's default database path is specific to macOS; use `SESSION_FINDER_CURSOR_DB` to point at its database on Linux.

### Prebuilt binaries

Download the archive for your machine and `SHA256SUMS` from [GitHub Releases](https://github.com/elichai/session-finder/releases). Releases are published when a version tag is pushed; there may be no release until the first tag.

| System | Archive |
| --- | --- |
| macOS Intel (11+) | `session-finder-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon (11+) | `session-finder-aarch64-apple-darwin.tar.gz` |
| Linux x86_64 | `session-finder-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 | `session-finder-aarch64-unknown-linux-musl.tar.gz` |

Linux binaries use static musl and bundled SQLite, so they do not require a matching glibc or a SQLite shared library. macOS binaries use only operating system libraries. The macOS deployment target is 11.0; CI runs on macOS 15, not every supported older OS. macOS archives are not signed or notarized.

For example, on Apple Silicon, from the download directory:

```sh
shasum -a 256 --ignore-missing -c SHA256SUMS
tar -xzf session-finder-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin
install -m 755 session-finder ~/.local/bin/session-finder
~/.local/bin/session-finder --help
```

On Linux, use `sha256sum --ignore-missing -c SHA256SUMS` and choose the Linux archive. Add `~/.local/bin` to your `PATH` if needed. `fzf` remains a separate runtime dependency for the panel.

### Build from source

Requires stable Rust and a C compiler for bundled SQLite. On Debian/Ubuntu, install the compiler with `sudo apt-get install build-essential`.

From the repository directory:

```sh
cargo install --path .
session-finder
```

On macOS, press Enter to copy and print the resume command, or Ctrl-O to open the selected chat in its app. If clipboard copying is unavailable, the command is still printed. Press Esc to close the panel. The detail pane shows the full working directory and session ID. A `*` marks an archived chat.

A two-line key guide stays visible above the results. It lists copy, open, refresh, assistant search, and close, plus the archived-chat marker. The assistant-search state stays visible when toggled.

The bottom pane updates automatically as you type or move between sessions. It shows a highlighted matching passage, with nearby context and a speaker label. With an empty query, it shows the opening user message. Long passages are excerpted; fuzzy highlighting shows a valid subsequence, which can differ from fzf's preferred alignment.

Press **Ctrl-A** to include assistant answers in search, and press it again to return to user-only search. The header shows `assistant ON` or `OFF`; every launch starts with it off. Assistant answers are always indexed, so toggling does not reread history. Tool output and reasoning blocks are excluded.

Press **Ctrl-R** to refresh history without closing the panel. The query and assistant-search mode stay in place. The selected chat is tracked by provider and session ID while the list is refreshed. Missing or no-longer-matching chats cannot remain selected. Esc and Ctrl-C close normally; picker failures return an error.

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

The Claude/Codex index is saved at **`~/.cache/session-finder/index.json`** on both macOS and Linux. It stores each source file's size and modification time, plus the session ID, working directory, activity time, preview, user messages, and assistant answers. It is a local JSON cache, not a background service.

The first run reads Claude history from `~/.claude/projects`, Codex history from `~/.codex/sessions`, and archived Codex history from `~/.codex/archived_sessions`. Internal Codex review and subagent runs are excluded. Each later launch reuses unchanged entries, reparses new or changed files, and drops deleted files automatically. **No manual rebuild is needed for normal use.** `--rebuild` forces a full reparse; a missing, invalid, or incompatible cache is rebuilt automatically.

Cursor's desktop SQLite database is read on every launch, using a consistent read-only snapshot that includes committed WAL updates. Session metadata supplies the exact workspace or worktree path and latest activity time. Subagent chats and empty drafts are excluded. Missing Cursor history is fine; a database error is reported while the other providers remain searchable.

Cursor sessions are not written to the JSON cache. An open panel keeps its current snapshot until Ctrl-R refreshes it. Search includes user messages, session IDs, and full working directories; Cursor chat titles are also searchable. Ctrl-A adds assistant answers to the search.

The cache contains user and assistant text in plain text, including assistant answers when assistant search is off. New cache directories are created with mode `0700`, and the cache file with mode `0600`. Cache updates use a temporary file and rename it into place. Session Finder does not upload history or the cache. The first launch after this update rebuilds older caches automatically.

To change the cache location:

```sh
SESSION_FINDER_CACHE=/path/to/private/index.json session-finder
```

`SESSION_FINDER_CLAUDE_ROOT`, `SESSION_FINDER_CODEX_ROOT`, and `SESSION_FINDER_CODEX_ARCHIVE_ROOT` override history locations. `SESSION_FINDER_CURSOR_DB` overrides the Cursor database path, which defaults to `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`.

## Tests

Run parser tests with `cargo test`, and helper tests with `node --test cursor-extension/extension.test.js`.

## Releasing

The Portable binaries workflow builds and tests all four targets on pull requests and manual runs, then uploads archives as workflow artifacts. Linux checks reject ELF interpreters or shared-library dependencies; macOS checks reject non-system dylibs. No release is published for a pull request or manual branch run.

To publish, merge the release changes, set the version in `Cargo.toml`, update `Cargo.lock` with `cargo check`, and push those changes. Then tag that commit (for example, `git tag v0.1.0` followed by `git push origin v0.1.0`), or publish a release with a new tag from GitHub's Releases page. The tag must match the Cargo version. Once every build passes, the tag run uploads all four archives and `SHA256SUMS`. If a release already exists, its title and notes are preserved; otherwise, the workflow creates it with generated notes. Uploads fail on duplicate asset names instead of replacing published files.
