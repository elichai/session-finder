use std::path::PathBuf;

const UNKNOWN_ARGUMENT: &str = "unknown argument; use --help";

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) enum Action {
    #[default]
    Pick,
    Help,
    List,
    Print(String),
    Open(String),
    PickerRows,
    PickerExtensionRows,
    PickerHeader,
    PickerPreview {
        path: PathBuf,
        fuzzy: bool,
        query: String,
    },
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Args {
    pub action: Action,
    pub rebuild: bool,
    pub fuzzy: bool,
    pub vscode: Option<crate::vscode::Editor>,
}

impl Args {
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut arguments = arguments.into_iter().peekable();
        let mut parsed = Self::default();
        // Picker helpers have fixed arguments; their query may itself be a flag.
        match arguments.peek().map(String::as_str) {
            Some("--picker-header" | "--picker-rows" | "--picker-extension-rows") => {
                parsed.action = match arguments.next().as_deref() {
                    Some("--picker-header") => Action::PickerHeader,
                    Some("--picker-extension-rows") => Action::PickerExtensionRows,
                    _ => Action::PickerRows,
                };
                return if arguments.next().is_none() {
                    Ok(parsed)
                } else {
                    Err(UNKNOWN_ARGUMENT)
                };
            }
            Some("--picker-preview") => {
                arguments.next();
                let path = arguments.next().ok_or(UNKNOWN_ARGUMENT)?.into();
                let mode = arguments.next().ok_or(UNKNOWN_ARGUMENT)?;
                let query = arguments.next().ok_or(UNKNOWN_ARGUMENT)?;
                if !matches!(mode.as_str(), "exact" | "fuzzy") || arguments.next().is_some() {
                    return Err(UNKNOWN_ARGUMENT);
                }
                parsed.action = Action::PickerPreview {
                    path,
                    fuzzy: mode == "fuzzy",
                    query,
                };
                return Ok(parsed);
            }
            _ => (),
        }

        let mut help = false;
        let mut actions = 0;
        let mut unknown = false;
        let mut missing_id = false;
        let mut conflicting_editors = false;
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "-h" | "--help" => help = true,
                "--rebuild" => parsed.rebuild = true,
                "--fuzzy" => parsed.fuzzy = true,
                "--vscode" | "--vscode-insiders" => {
                    let editor = if argument == "--vscode" {
                        crate::vscode::Editor::Code
                    } else {
                        crate::vscode::Editor::Insiders
                    };
                    conflicting_editors |= parsed.vscode.is_some_and(|previous| previous != editor);
                    parsed.vscode = Some(editor);
                }
                "--list" => {
                    actions += 1;
                    parsed.action = Action::List;
                }
                "--print" | "--open" => {
                    actions += 1;
                    if let Some(id) = arguments.next_if(|id| !id.starts_with('-')) {
                        parsed.action = if argument == "--print" {
                            Action::Print(id)
                        } else {
                            Action::Open(id)
                        };
                    } else {
                        missing_id = true;
                    }
                }
                _ => unknown = true,
            }
        }
        if help {
            parsed.action = Action::Help;
        } else if conflicting_editors {
            return Err("choose one of --vscode or --vscode-insiders");
        } else if actions > 1 {
            return Err("choose one of --list, --print, or --open");
        } else if unknown {
            return Err(UNKNOWN_ARGUMENT);
        } else if missing_id {
            return Err("--print and --open need a session ID");
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vscode_modes_work_with_print_and_reject_conflicting_editors() {
        for mode in ["--vscode", "--vscode-insiders"] {
            assert!(Args::parse([mode, "--print", "session-id"].map(str::to_owned)).is_ok());
        }
        assert!(Args::parse(["--vscode", "--vscode-insiders"].map(str::to_owned)).is_err());
    }

    #[test]
    fn parses_actions_and_flags_in_either_order() {
        for arguments in [
            ["--rebuild", "--print", "session-id", "--fuzzy"],
            ["--print", "session-id", "--fuzzy", "--rebuild"],
        ] {
            assert_eq!(
                Args::parse(arguments.map(str::to_owned)).unwrap(),
                Args {
                    action: Action::Print("session-id".into()),
                    rebuild: true,
                    fuzzy: true,
                    vscode: None,
                }
            );
        }
        assert_eq!(Args::parse([]).unwrap(), Args::default());
        assert_eq!(
            Args::parse(["--open", "session-id"].map(str::to_owned))
                .unwrap()
                .action,
            Action::Open("session-id".into())
        );
    }

    #[test]
    fn helper_queries_are_not_parsed_as_options() {
        assert_eq!(
            Args::parse(["--picker-preview", "/row", "fuzzy", "--help"].map(str::to_owned))
                .unwrap()
                .action,
            Action::PickerPreview {
                path: "/row".into(),
                fuzzy: true,
                query: "--help".into(),
            }
        );
    }
}
