//! The `openshop` binary, run as a user runs it.

use std::process::{Command, Output};

fn openshop(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_openshop"))
        .args(args)
        .output()
        .expect("the binary runs")
}

#[test]
fn an_unknown_command_fails_with_the_usage_on_stderr() {
    for command in ["frobnicate", "inti", "--version-please"] {
        let output = openshop(&[command]);
        assert_eq!(output.status.code(), Some(2), "{command}");
        assert!(output.stdout.is_empty(), "{command}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with(&format!("[openshop] Unknown command: {command}\n")),
            "{stderr}"
        );
        assert!(stderr.contains("Commands:"), "{stderr}");
    }
}

#[test]
fn no_command_or_a_request_for_help_prints_the_usage_and_succeeds() {
    for args in [&[][..], &["help"], &["--help"], &["-h"]] {
        let output = openshop(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Commands:"),
            "{args:?}"
        );
        assert!(output.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn the_usage_names_the_commands_that_run_a_flow_by_hand() {
    let usage = String::from_utf8_lossy(&openshop(&["--help"]).stdout).into_owned();
    assert!(
        usage.contains("run <flow> [--shop=<domain> | --all-shops] [--input=<json>]"),
        "{usage}"
    );
    assert!(usage.contains("cron run <name>"), "{usage}");
}
