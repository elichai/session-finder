# session-finder

Find a local Claude Code, Codex, GitHub Copilot CLI, VS Code chat, or Cursor conversation by its working directory or by text you wrote in it. VS Code support includes Copilot and other extensions that use its native Chat view. The panel starts with the most recently used session. Search matches the typed phrase; run `session-finder --fuzzy` for looser subsequence matching.

`fzf` still reads `FZF_DEFAULT_OPTS` for your preferred appearance and key bindings. The tool sets its search mode explicitly. Both modes use literal characters and smart case: lowercase queries ignore case; a query containing uppercase letters is case-sensitive. Accents are distinct, and extended-search operators are disabled, including with `--fuzzy`.

## Install

Requires **fzf 0.71.0 or later**. The picker uses field-based selection tracking across refreshes, added in fzf 0.71.0. SQLite is bundled into the binary; no system SQLite installation is needed. Install the Claude, Codex, or Copilot CLI to resume that provider in a terminal.

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

Linux supports the Claude/Codex/Copilot/VS Code history panel and printed terminal resume commands. Clipboard copying and Codex/Cursor GUI opening currently use macOS tools (`pbcopy` and `open`). The Cursor helper installation instructions below also target macOS. Cursor's default database path is specific to macOS; use `SESSION_FINDER_CURSOR_DB` to point at its database on Linux.

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

For GitHub Copilot CLI, Enter copies `cd -- 'PWD' && copilot --resume='ID'`. Both the working directory and session ID are shell-quoted. Copilot sessions use terminal resume; Ctrl-O and `--open` report that desktop opening is unavailable. This reads Copilot CLI history; VS Code Copilot Chat is listed separately as `VSCode`.

For saved native VS Code chats, including Copilot, **Ctrl-O opens the exact chat in its recorded workspace**. This uses VS Code's built-in session route and needs no Session Finder helper extension or extra mode flag. Stable sessions use `code`; Insiders sessions use `code-insiders`. The opening command selects the source user-data directory, including a custom directory with the standard `User/workspaceStorage` layout, so it reads the correct saved chat. Multi-root workspaces keep their exact `.code-workspace` target.

Enter copies a `session-finder --open 'VSCode:ID'` command using this binary's path. Session Finder coordinates editor startup before opening the chat, because Code's initial URL handling can discard a session parameter. If Code is closed, the tool starts the recorded workspace, waits for the runtime URL handler, then restores the chat. When Code is already running, it opens a new target window and leaves the current workspace open. VS Code may show its standard local-file protocol confirmation; Session Finder does not change that security setting.

Exact opening requires a native saved session and recorded local workspace metadata. Hook-only recovery transcripts, empty-window chats, unknown workspaces, remote-server history, and nonstandard storage layouts remain searchable. Enter retains a workspace-opening command where possible; Ctrl-O reports that exact restoration is unavailable. If no local target is known, Enter prints a shell comment naming the session. Deleted saves are checked before opening; a concurrent file change can still affect Code's own asynchronous restore.

The preview shows the editor, workspace target, title, and chat participants; these details are searchable with assistant search off. An explicit `--open 'VSCode:ID'` also disambiguates IDs shared with other providers.

### Claude Code and Codex in VS Code

The standalone Claude Code and Codex extensions share their CLI's history. Their conversations appear as `Claude` and `Codex`, once per session, including conversations continued in both the terminal and the editor. They do not use the native `VSCode` chat reader described above.

To copy an extension session link or open it in VS Code:

```sh
session-finder --vscode                     # Enter copies the link command; Ctrl-O opens it
session-finder --vscode --print SESSION_ID
session-finder --vscode --open SESSION_ID
```

Use `--vscode-insiders` instead for Insiders. These modes require `code` or `code-insiders` in `PATH` and the matching extension installed. The panel shows only Claude and Codex sessions in these modes, including after refresh. Without either flag, Enter still copies terminal resume commands and Ctrl-O still opens the desktop app.

**Focus the session's workspace in the editor first.** The links select an existing session ID but do not switch workspaces. [Claude's documented link](https://code.claude.com/docs/en/vs-code#launch-a-vs-code-tab-from-other-tools) can silently start a new conversation if that session is not available in the focused workspace. Codex uses the installed extension's `/local/<id>` route; that route is internal and can change with extension updates. The tool prints the workspace reminder when copying or opening.

Claude's hidden metadata and known generated leading editor-context block are excluded from search. Literal tag examples are preserved. Session relocation records update the reported working directory. No extension settings files are scanned: if an extension sets a custom home only in its VS Code settings, set the same home environment variable when running Session Finder, or use the explicit history-root override below. Remote extension history must be available locally through an override; this tool does not connect to remote hosts.

### Cursor

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

The Claude/Codex/Copilot/VS Code index is saved at **`~/.cache/session-finder/index.json`** on both macOS and Linux. It stores each source file's size and modification time, plus the session ID, working directory, activity time, preview, user messages, and assistant answers. It is a local JSON cache, not a background service.

The first run reads Claude history from `$CLAUDE_CONFIG_DIR/projects` (default `~/.claude/projects`), Codex history from `$CODEX_HOME/sessions` (default `~/.codex/sessions`), and archived Codex history from `$CODEX_HOME/archived_sessions`. Internal Claude sidechains and Codex review/subagent runs are excluded. Each later launch reuses unchanged entries, reparses new or changed files, and drops deleted files automatically. **No manual rebuild is needed for normal use.** `--rebuild` forces a full reparse; a missing, invalid, or incompatible cache is rebuilt automatically.

Copilot CLI history is read from `~/.copilot/session-state/<id>/events.jsonl`, or `$COPILOT_HOME/session-state` when configured. The reader indexes user messages and assistant answers, uses the latest recorded working directory and event timestamp, and excludes subagent messages, tool output, and reasoning. Custom session IDs are supported. Sessions without a recorded absolute directory or a user message are skipped. The older `history-session-state` format is not supported.

VS Code history is read from `<workspaceStorage>/<hash>/chatSessions/<id>.json` and `.jsonl`. The reader supports legacy snapshots and current snapshot/update journals, including nested sets, array updates, and deletions. It extracts human prompts and assistant text across native Chat participants, including Copilot; tool output, reasoning, hidden turns, and unsent drafts are excluded. Extensions with their own chat storage need a separate reader.

By default it scans VS Code and Insiders on Linux (`~/.config/Code*/User`) and macOS (`~/Library/Application Support/Code*/User`), plus Remote servers (`~/.vscode-server*/data/User`). Chats from empty windows are read from the sibling `globalStorage/emptyWindowChatSessions` directory. Only known chat directories are scanned. The scanner holds one directory handle even with many workspaces.

Copilot hook transcripts under `GitHub.copilot-chat/transcripts/<id>.jsonl` can recover a session when no valid native saved session is available. They are supplemental: VS Code creates them only for hook-enabled requests and prunes old transcripts. Each session appears once. A valid native journal takes precedence over a legacy snapshot, which takes precedence over a hook transcript; older copies cannot restore deleted text in the selected native session. A valid empty or fully hidden native session suppresses older copies and is omitted from the results. An incomplete final journal record is ignored while a live write is in progress. A corrupt journal is skipped, allowing another valid saved copy or transcript to supply the session.

The working directory comes from native session metadata or `workspace.json` when available. Workspace metadata is read again on each launch or Ctrl-R refresh, even when the conversation text comes from cache. Adding, editing, or removing `workspace.json` takes effect without `--rebuild`. The first launch after this update rebuilds the older cache once.

If reading a Claude, Codex, Copilot, or VS Code session file fails, the load or refresh stops with an error and leaves the existing cache unchanged. Files deleted during indexing are skipped.

Cursor's desktop SQLite database is read on every launch, using a consistent read-only snapshot that includes committed WAL updates. Session metadata supplies the exact workspace or worktree path and latest activity time. Subagent chats and empty drafts are excluded. Missing Cursor history is fine; a database error is reported while the other providers remain searchable.

Cursor sessions are not written to the JSON cache. An open panel keeps its current snapshot until Ctrl-R refreshes it. Search includes user messages, session IDs, and full working directories; Cursor chat titles and VS Code workspace targets, titles, and participant IDs are also searchable. Ctrl-A adds assistant answers to the search.

The cache contains user and assistant text in plain text, including assistant answers when assistant search is off. New cache directories are created with mode `0700`, and the cache file with mode `0600`. Cache updates use a temporary file and rename it into place. Session Finder does not upload history or the cache. The first launch after this update rebuilds older caches automatically.

To change the cache location:

```sh
SESSION_FINDER_CACHE=/path/to/private/index.json session-finder
```

`SESSION_FINDER_CLAUDE_ROOT`, `SESSION_FINDER_CODEX_ROOT`, and `SESSION_FINDER_CODEX_ARCHIVE_ROOT` override history locations, taking precedence over the provider home variables. Keep `CLAUDE_CONFIG_DIR` or `CODEX_HOME` set when running a copied terminal resume command for a custom home. `SESSION_FINDER_CURSOR_DB` overrides the Cursor database path, which defaults to `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`.

`SESSION_FINDER_COPILOT_ROOT` overrides the Copilot `session-state` directory directly. When resuming sessions from a non-default Copilot home, keep `COPILOT_HOME` set in the terminal where you run the copied command.

`SESSION_FINDER_VSCODE_ROOT` replaces the default VS Code `workspaceStorage` directories with a colon-separated list, including for custom user-data locations. Each root also supplies its sibling `globalStorage/emptyWindowChatSessions` directory. The command uses `code-insiders` when the storage path contains `Code - Insiders` or `.vscode-server-insiders`.

## Tests

Run parser tests with `cargo test`, and helper tests with `node --test cursor-extension/extension.test.js`.

## Releasing

The Portable binaries workflow builds and tests all four targets on pull requests and manual runs, then uploads archives as workflow artifacts. Linux checks reject ELF interpreters or shared-library dependencies; macOS checks reject non-system dylibs. No release is published for a pull request or manual branch run.

To publish, merge the release changes, set the version in `Cargo.toml`, update `Cargo.lock` with `cargo check`, and push those changes. Then tag that commit (for example, `git tag v0.1.0` followed by `git push origin v0.1.0`), or publish a release with a new tag from GitHub's Releases page. The tag must match the Cargo version. Once every build passes, the tag run uploads all four archives and `SHA256SUMS`. If a release already exists, its title and notes are preserved; otherwise, the workflow creates it with generated notes. Uploads fail on duplicate asset names instead of replacing published files.
