use crate::{
    age, clean, display_cwd, display_source, shell_quote, source_name, SelectionAction, Session,
};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

const HEADER: &str = "Enter: copy • Ctrl-O: open • Esc/Ctrl-C: close • *: archived\n\
Ctrl-R: refresh • Ctrl-A: assistant";

fn includes_assistant() -> bool {
    std::env::var("FZF_WITH_NTH").as_deref() == Ok("2..7")
}

pub fn print_header() {
    println!(
        "{HEADER} {}",
        if includes_assistant() { "ON" } else { "OFF" }
    );
}

pub fn print_preview(path: &Path, query: &str, fuzzy: bool) -> io::Result<()> {
    let row = fs::read_to_string(path)?;
    print!("{}", preview(&row, query, fuzzy, includes_assistant())?);
    Ok(())
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
        if source == "Copilot" {
            !id.is_empty() && !id.chars().any(char::is_control)
        } else {
            matches!(source, "Claude" | "Codex" | "Cursor") && crate::codex::is_uuid(id)
        }
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
    if fuzzy {
        // shortcut: highlight a valid subsequence; use fzf's scoring offsets if it exposes them.
        let mut text = text.chars().map(fold).enumerate();
        return query
            .chars()
            .map(fold)
            .map(|ch| {
                text.find(|&(_, candidate)| candidate == ch)
                    .map(|(index, _)| index)
            })
            .collect();
    }
    if sensitive {
        return text.find(query).map(|start| {
            let start = text[..start].chars().count();
            (start..start + query.chars().count()).collect()
        });
    }
    if text.is_ascii() && query.is_ascii() {
        return text
            .as_bytes()
            .windows(query.len())
            .position(|part| part.eq_ignore_ascii_case(query.as_bytes()))
            .map(|start| (start..start + query.len()).collect());
    }
    let text: Vec<_> = text.chars().map(fold).collect();
    let query: Vec<_> = query.chars().map(fold).collect();
    text.windows(query.len())
        .position(|part| part == query)
        .map(|start| (start..start + query.len()).collect())
}

fn excerpt(text: &str, positions: impl DoubleEndedIterator<Item = usize> + Clone) -> String {
    let start = positions.clone().next().unwrap().saturating_sub(60);
    let end = (positions.clone().next_back().unwrap() + 80).min(start + 400);
    let mut positions = positions.peekable();
    let mut chars = text.chars().enumerate();
    let mut result = String::with_capacity(text.len().min(400));
    if start > 0 {
        result.push('…');
    }
    let mut highlighted = false;
    for (index, ch) in chars.by_ref().take(end).skip(start) {
        let matched = positions.next_if_eq(&index).is_some();
        if matched != highlighted {
            result.push_str(if matched { "\x1b[1;33m" } else { "\x1b[0m" });
            highlighted = matched;
        }
        result.push(ch);
    }
    if highlighted {
        result.push_str("\x1b[0m");
    }
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn preview(row: &str, query: &str, fuzzy: bool, assistant: bool) -> io::Result<String> {
    let row = row.trim_end_matches('\n');
    if row.is_empty() {
        return Ok(String::new());
    }
    let mut fields = row.split('\t').map(|field| clean(field, usize::MAX));
    let values: [Option<String>; 7] = std::array::from_fn(|_| fields.next());
    if values.iter().any(Option::is_none) || fields.next().is_some() {
        return Err(io::Error::other("invalid picker row"));
    }
    let fields = values.map(Option::unwrap);
    let mut output = format!("PWD: {}\nID: {}\n\n", fields[4], fields[5]);
    if fields[0].starts_with("Copilot:") {
        output.push_str("Copilot CLI: Enter copies a terminal resume command; desktop opening is unavailable.\n");
    }
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
                excerpt(text, positions.iter().copied())
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
        let start = positions.partition_point(|&position| position < offset);
        let end = positions.partition_point(|&position| position < offset + len);
        let local = &positions[start..end];
        if !local.is_empty() {
            output.push_str(&format!(
                "Matching {label}:\n{}\n",
                excerpt(text, local.iter().map(|position| position - offset))
            ));
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
    fn copilot_selections_accept_custom_ids_and_explain_terminal_resume() {
        assert_eq!(
            selection(ExitStatus::from_raw(0), b"\nCopilot:work-task_123\n").unwrap(),
            Some(("Copilot:work-task_123".into(), SelectionAction::Copy))
        );
        for value in [
            b"\nCopilot:\n".as_slice(),
            b"\nCopilot:a\tb\n",
            b"\nCopilot:a\nb\n",
        ] {
            assert!(selection(ExitStatus::from_raw(0), value).is_err());
        }
        let text = preview(
            "Copilot:work-task_123\tnow\topening\trequest\t/work\twork-task_123\tanswer",
            "",
            false,
            false,
        )
        .unwrap();
        assert!(text.contains("Enter copies a terminal resume command"));
    }

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
        assert_eq!(match_positions("αaβαβ", "αββ", true), Some(vec![0, 2, 4]));
        assert_eq!(match_positions("İX", "ix", true), Some(vec![0, 1]));
        assert_eq!(match_positions("İX", "iX", true), None);
        assert_eq!(match_positions("İX", "ix", false), Some(vec![0, 1]));
        assert_eq!(match_positions("α Hello", "H", false), Some(vec![2]));
        assert_eq!(
            match_positions("a Αβ Hello", "Αβ H", false),
            Some(vec![2, 3, 4, 5])
        );
        assert_eq!(match_positions("anything", "", true), Some(vec![]));
        assert_eq!(match_positions("anything", "", false), Some(vec![]));
    }

    #[test]
    fn excerpts_preserve_unicode_highlights_at_the_end_and_limit_long_spans() {
        assert_eq!(
            excerpt("αβγδ", [1, 3].into_iter()),
            "α\x1b[1;33mβ\x1b[0mγ\x1b[1;33mδ\x1b[0m"
        );
        let output = excerpt(&"x".repeat(500), [0, 450].into_iter());
        assert!(output.ends_with('…'));
        assert_eq!(output.matches("\x1b[1;33m").count(), 1);
        assert_eq!(
            output
                .replace("\x1b[1;33m", "")
                .replace("\x1b[0m", "")
                .chars()
                .count(),
            401
        );
    }

    #[test]
    fn picker_rows_require_seven_fields_but_allow_an_empty_assistant_field() {
        for row in ["a\tb\tc\td\te\tf", "a\tb\tc\td\te\tf\tg\th"] {
            assert!(preview(row, "query", false, false).is_err());
        }
        assert!(
            preview("id\tnow\topening\tuser\t/work\tid\t", "", false, false)
                .unwrap()
                .contains("Opening user message: opening")
        );
    }

    #[test]
    fn fuzzy_metadata_matches_highlight_each_field_with_its_local_positions() {
        let output = preview("Claude:id\tα\tβ\tγ\t/work\tid\tδ", "αβ", true, false).unwrap();
        assert!(output.contains("Matching Session:\n\x1b[1;33mα\x1b[0m"));
        assert!(output.contains("Matching Opening user message:\n\x1b[1;33mβ\x1b[0m"));
        assert_eq!(output.matches("Opening user message").count(), 1);
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
