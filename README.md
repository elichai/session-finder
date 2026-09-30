# session-finder

Find a local Claude or Codex conversation by its working directory or by text you wrote in it. The panel starts with the most recently used session. Search matches the typed phrase; run `session-finder --fuzzy` for looser subsequence matching.

`fzf` still reads `FZF_DEFAULT_OPTS` for your preferred appearance and key bindings. The tool sets its search mode explicitly.

## Install

Requires Rust, `fzf`, and the Claude and/or Codex CLI. On macOS, the selected resume command is copied with `pbcopy`.

```sh
cargo install --path .
session-finder
```

Press Enter to copy and print the resume command, or Ctrl-O to open a Codex chat in the GUI. Press Esc to close the panel. The detail pane shows the selected session's full working directory and ID. The command targets that ID and directory. A `Codex*` row is archived; Codex moves it to active history when the resume command runs. Ctrl-O requires the Codex desktop app on macOS and works only for Codex rows.

## Other commands

```sh
session-finder --list              # recent-first list
session-finder --print SESSION_ID  # print one resume command
session-finder --rebuild           # reindex every session file
session-finder --fuzzy             # looser matching
```

The first run reads all session files, including archived Codex sessions. Internal Codex review and subagent runs are excluded. Each later launch checks file size and modification time, reparses changed files, and drops deleted files automatically. `--rebuild` is only for forcing a full reparse. An already open panel keeps its launch-time snapshot; reopen it to see new messages. Search includes the text of user messages, session IDs, and full working directories. The cache is a private local JSON file at `~/.cache/session-finder/index.json`; it contains your messages in plain text. Set `SESSION_FINDER_CACHE` to use another path. `SESSION_FINDER_CLAUDE_ROOT`, `SESSION_FINDER_CODEX_ROOT`, and `SESSION_FINDER_CODEX_ARCHIVE_ROOT` can override the default history locations.
