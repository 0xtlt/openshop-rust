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

#[test]
fn help_after_a_command_of_the_cli_prints_the_usage_and_does_nothing_else() {
    let dir = std::env::temp_dir().join(format!("openshop-cli-help-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "--help"][..],
        &["init", "my-app", "-h"],
        &["init-ui", "--help"],
        &["init-ui", "-h"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_openshop"))
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("the binary runs");
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Commands:"),
            "{args:?}"
        );
        assert!(output.stderr.is_empty(), "{args:?}");
    }
    // Nothing was scaffolded.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn build_ui_is_no_longer_a_command() {
    // The admin is rendered by the server: there is no bundle to build.
    let output = Command::new(env!("CARGO_BIN_EXE_openshop"))
        .arg("build-ui")
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Unknown command: build-ui"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
