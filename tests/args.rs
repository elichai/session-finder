use std::process::Command;

#[test]
fn invalid_arguments_fail_before_loading_history() {
    for (args, message) in [
        (vec!["--unknown"], "unknown argument"),
        (vec!["--print"], "need a session ID"),
        (vec!["--open", "--fuzzy"], "need a session ID"),
        (vec!["--list", "--list"], "choose one"),
        (vec!["--list", "--print", "id"], "choose one"),
        (vec!["--print", "id", "--open", "id"], "choose one"),
        (vec!["--picker-header", "extra"], "unknown argument"),
        (
            vec!["--picker-preview", "path", "exact"],
            "unknown argument",
        ),
        (
            vec!["--picker-preview", "path", "other", "query"],
            "unknown argument",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(&args)
            .env_remove("HOME")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(message), "{args:?}: {error}");
        assert!(!error.contains("HOME"), "{args:?}: {error}");
    }
}

#[test]
fn help_takes_precedence_over_invalid_public_arguments() {
    for args in [
        vec!["--help"],
        vec!["--unknown", "-h"],
        vec!["--print", "--help"],
        vec!["--list", "--open", "id", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(&args)
            .env_remove("HOME")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("Usage: session-finder"));
    }
}
