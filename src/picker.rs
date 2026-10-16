use crate::{
    age, clean, display_cwd, display_source, shell_quote, source_name, SelectionAction, Session,
};
use std::fs;
use std::io::{self, Write};
use std::process::{Command, ExitStatus, Stdio};

const HEADER: &str = "Enter: copy • Ctrl-O: open • Esc/Ctrl-C: close • *: archived\n\
Ctrl-R: refresh • Ctrl-A: assistant";

fn includes_assistant() -> bool {
    std::env::var("FZF_WITH_NTH").as_deref() == Ok("2..7")
}

pub fn run_helper(args: &[String]) -> io::Result<bool> {
    match args {
        [flag] if flag == "--picker-header" => {
            println!(
                "{HEADER} {}",
                if includes_assistant() { "ON" } else { "OFF" }
            );
        }
        [flag, path, mode, query]
            if flag == "--picker-preview" && matches!(mode.as_str(), "exact" | "fuzzy") =>
        {
            let row = fs::read_to_string(path)?;
            print!(
                "{}",
                preview(&row, query, mode == "fuzzy", includes_assistant())?
            );
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub fn identity(session: &Session) -> String {
    format!("{}:{}", source_name(session.source), session.id)
}

pub fn write_rows(sessions: &[Session], output: &mut impl Write) -> io::Result<()> {
    for session in sessions {
        writeln!(
            output,
            "{}\t{:>8}  {:<7}  {:<28}\t{:<60}\t{}\t{}\t{}\t{}",
            identity(session),
            age(session.updated),
            display_source(session),
            display_cwd(&session.cwd),
            clean(&session.preview, 60),
            clean(&session.search_text, usize::MAX),
            clean(&session.cwd.to_string_lossy(), usize::MAX),
            clean(&session.id, usize::MAX),
            clean(&session.assistant_text, usize::MAX)
        )?;
    }
    Ok(())
}

fn command(fuzzy: bool) -> io::Result<Command> {
    let executable = std::env::current_exe()?;
    let executable = shell_quote(&executable.to_string_lossy());
    let mut picker = Command::new("fzf");
    picker.args([
        "--delimiter=\t",
        "--with-nth=2..6",
        "--nth=..",
        "--accept-nth=1",
        "--expect=ctrl-o",
        "--no-multi",
        "--no-sort",
        "--track",
        "--id-nth=1",
        "--freeze-left=2",
        "--layout=reverse",
        "--border",
        "--height=90%",
        "--prompt=Sessions> ",
        "--preview-window=down,7,wrap",
        "--preview-label=Selected session",
        "--no-extended",
        "--literal",
        "--smart-case",
        if fuzzy { "+e" } else { "--exact" },
    ]);
    picker.arg(format!("--header={HEADER} OFF"));
    picker.arg(format!(
        "--preview={executable} --picker-preview {{f}} {} {{q}}",
        if fuzzy { "fuzzy" } else { "exact" }
    ));
    picker.arg(format!("--bind=ctrl-a:change-with-nth(2..7|2..6)+transform-header({executable} --picker-header)+refresh-preview"));
    picker.arg(format!(
        "--bind=ctrl-r:reload-sync({executable} --picker-rows)+refresh-preview"
    ));
    Ok(picker)
}

pub fn choose(sessions: &[Session], fuzzy: bool) -> io::Result<Option<(String, SelectionAction)>> {
    let mut child = command(fuzzy)?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = write_rows(sessions, &mut stdin) {
            if error.kind() != io::ErrorKind::BrokenPipe {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
    let output = child.wait_with_output()?;
    selection(output.status, &output.stdout)
}

fn selection(status: ExitStatus, output: &[u8]) -> io::Result<Option<(String, SelectionAction)>> {
    match status.code() {
        Some(1 | 130) => return Ok(None),
        Some(0) => (),
        _ => {
            return Err(io::Error::other(format!(
                "fzf failed ({status}); requires fzf 0.71.0 or later"
            )))
        }
    }
    let output = std::str::from_utf8(output).map_err(io::Error::other)?;
    let mut lines = output.lines();
    let action = match lines.next() {
        Some("") => SelectionAction::Copy,
        Some("ctrl-o") => SelectionAction::OpenGui,
        _ => return Err(io::Error::other("fzf returned an unknown action")),
    };
    let key = lines
        .next()
        .ok_or_else(|| io::Error::other("fzf returned no selection"))?;
    let valid = key.split_once(':').is_some_and(|(source, id)| {
        matches!(source, "Claude" | "Codex" | "Cursor") && crate::codex::is_uuid(id)
    });
    if !valid || lines.next().is_some() {
        return Err(io::Error::other("fzf returned an invalid selection"));
    }
    Ok(Some((key.to_owned(), action)))
}

fn match_positions(text: &str, query: &str, fuzzy: bool) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let sensitive = query.chars().any(char::is_uppercase);
    let fold = |ch: char| {
        if sensitive {
            ch
        } else {
            ch.to_lowercase().next().unwrap_or(ch)
        }
    };
    let text: Vec<_> = text.chars().map(fold).collect();
    let query: Vec<_> = query.chars().map(fold).collect();
    if !fuzzy {
        return text
            .windows(query.len())
            .position(|part| part == query)
            .map(|start| (start..start + query.len()).collect());
    }
    // shortcut: highlight a valid subsequence; use fzf's scoring offsets if it exposes them.
    let mut positions = Vec::new();
    let mut start = 0;
    for ch in query {
        let index = start
            + text[start..]
                .iter()
                .position(|&candidate| candidate == ch)?;
        positions.push(index);
        start = index + 1;
    }
    Some(positions)
}

fn excerpt(text: &str, positions: &[usize]) -> String {
    let chars: Vec<_> = text.chars().collect();
    let start = positions[0].saturating_sub(60);
    let end = (positions[positions.len() - 1] + 80)
        .min(chars.len())
        .min(start + 400);
    let mut result = if start > 0 {
        "…".to_owned()
    } else {
        String::new()
    };
    let mut highlighted = false;
    for (index, &ch) in chars.iter().enumerate().take(end).skip(start) {
        let matched = positions.binary_search(&index).is_ok();
        if matched != highlighted {
            result.push_str(if matched { "\x1b[1;33m" } else { "\x1b[0m" });
            highlighted = matched;
        }
        result.push(ch);
    }
    if highlighted {
        result.push_str("\x1b[0m");
    }
    if end < chars.len() {
        result.push('…');
    }
    result
}

fn preview(row: &str, query: &str, fuzzy: bool, assistant: bool) -> io::Result<String> {
    let row = row.trim_end_matches('\n');
    if row.is_empty() {
        return Ok(String::new());
    }
    let fields: Vec<_> = row
        .split('\t')
        .map(|field| clean(field, usize::MAX))
        .collect();
    if fields.len() != 7 {
        return Err(io::Error::other("invalid picker row"));
    }
    let mut output = format!("PWD: {}\nID: {}\n\n", fields[4], fields[5]);
    if query.is_empty() {
        output.push_str(&format!("Opening user message: {}\n", fields[2]));
        return Ok(output);
    }
    for (label, text) in [("User", &fields[3]), ("Assistant", &fields[6])]
        .iter()
        .take(if assistant { 2 } else { 1 })
    {
        if let Some(positions) = match_positions(text, query, fuzzy) {
            output.push_str(&format!(
                "Matching {label}:\n{}\n",
                excerpt(text, &positions)
            ));
            return Ok(output);
        }
    }
    let searchable = &fields[1..if assistant { 7 } else { 6 }];
    let Some(positions) = match_positions(&searchable.join("\t"), query, fuzzy) else {
        output.push_str(&format!("Opening user message: {}\n", fields[2]));
        return Ok(output);
    };
    let mut offset = 0;
    let mut message_match = false;
    for (label, text) in [
        "Session",
        "Opening user message",
        "User",
        "PWD",
        "ID",
        "Assistant",
    ]
    .iter()
    .zip(searchable)
    {
        let len = text.chars().count();
        let local: Vec<_> = positions
            .iter()
            .filter(|&&position| (offset..offset + len).contains(&position))
            .map(|position| position - offset)
            .collect();
        if !local.is_empty() {
            output.push_str(&format!("Matching {label}:\n{}\n", excerpt(text, &local)));
            message_match |= matches!(*label, "User" | "Assistant" | "Opening user message");
        }
        offset += len + 1;
    }
    if !message_match {
        output.push_str(&format!("Opening user message: {}\n", fields[2]));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn cancellation_is_normal_but_picker_errors_and_signals_are_errors() {
        for code in [1, 130] {
            assert!(selection(ExitStatus::from_raw(code << 8), b"")
                .unwrap()
                .is_none());
        }
        for raw in [2 << 8, 127 << 8, 9] {
            assert!(selection(ExitStatus::from_raw(raw), b"").is_err());
        }
        let key = "Codex:123e4567-e89b-12d3-a456-426614174000";
        for (prefix, action) in [
            ("", SelectionAction::Copy),
            ("ctrl-o", SelectionAction::OpenGui),
        ] {
            assert_eq!(
                selection(
                    ExitStatus::from_raw(0),
                    format!("{prefix}\n{key}\n").as_bytes()
                )
                .unwrap(),
                Some((key.into(), action))
            );
        }
        assert!(selection(ExitStatus::from_raw(0), b"\ninvalid\n").is_err());
    }

    #[test]
    fn matching_is_literal_smart_case_and_unicode_safe() {
        assert_eq!(match_positions("αβ hello", "β H", false), None);
        assert_eq!(
            match_positions("αβ Hello", "β h", false),
            Some(vec![1, 2, 3])
        );
        assert_eq!(match_positions("résumé", "resume", false), None);
        assert_eq!(match_positions("a.*b", ".*", false), Some(vec![1, 2]));
        assert_eq!(
            match_positions("Timing attack", "tmng", true),
            Some(vec![0, 2, 4, 5])
        );
        assert_eq!(match_positions("short", "longer query", true), None);
        assert_eq!(match_positions("short", "longer query", false), None);
    }

    #[test]
    fn excerpts_find_late_matches_and_strip_untrusted_terminal_controls() {
        let text = format!("{}needle\x1b[31m", "α".repeat(500));
        let row = format!("Claude:id\tnow\topening\t{text}\t/work\tid\tsecret");
        let output = preview(&row, "needle", false, false).unwrap();
        assert!(output.contains("…"));
        assert!(output.contains("\x1b[1;33mneedle\x1b[0m"));
        assert!(!output.contains("\x1b[31m"));
        assert!(output.chars().count() < 250);
        assert!(!output.contains("secret"));
    }

    #[test]
    fn metadata_matches_keep_the_opening_message_and_text_matches_use_full_context() {
        let row =
            "Claude:id\tClaude /work\topening\topening with useful context\t/work\tid\tsecret";
        let output = preview(row, "/work", false, false).unwrap();
        assert!(output.contains("Opening user message: opening"));
        let output = preview(row, "opening", false, false).unwrap();
        assert!(output.contains("Matching User:"));
        assert!(output.contains("with useful context"));
    }
}
