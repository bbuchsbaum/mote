use std::process::Command;

use clap::{Command as ClapCommand, CommandFactory};
use serde_json::Value;

use mote::cli::Cli;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn leaf_paths(command: &ClapCommand, parent: &mut Vec<String>, out: &mut Vec<String>) {
    let children: Vec<_> = command.get_subcommands().collect();
    if children.is_empty() {
        out.push(parent.join(" "));
        return;
    }
    for child in children {
        if child.get_name() == "help" {
            continue;
        }
        parent.push(child.get_name().to_string());
        leaf_paths(child, parent, out);
        parent.pop();
    }
}

#[test]
fn help_all_is_sorted_complete_and_includes_deep_agent_surfaces() {
    let output = Command::new(mote_bin())
        .args(["help", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let actual: Vec<_> = text
        .lines()
        .map(|line| line.split("  Usage:").next().unwrap().trim().to_string())
        .collect();

    let mut expected = Vec::new();
    leaf_paths(&Cli::command(), &mut Vec::new(), &mut expected);
    expected.sort();
    assert_eq!(
        actual, expected,
        "new Clap leaves must appear automatically"
    );
    assert!(actual.iter().any(|path| path == "actor list"));
    assert!(actual.iter().any(|path| path == "msg requests"));
    assert!(actual.iter().any(|path| path == "discuss supersede"));
    assert!(
        actual
            .iter()
            .any(|path| path == "candidate evidence record")
    );
    assert!(text.lines().all(|line| line.contains("  Usage:")));
}

#[test]
fn help_all_json_has_stable_machine_readable_leaf_records() {
    let output = Command::new(mote_bin())
        .args(["--json", "help", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = value.as_array().unwrap();
    assert!(!rows.is_empty());
    for row in rows {
        assert!(row["path"].is_string());
        assert!(row["usage"].as_str().unwrap().starts_with("Usage:"));
        assert!(row["about"].is_string());
    }
    assert!(
        rows.windows(2)
            .all(|pair| pair[0]["path"].as_str() < pair[1]["path"].as_str())
    );
}

#[test]
fn ordinary_help_still_renders_root_and_nested_commands() {
    for args in [
        vec!["help"],
        vec!["help", "actor", "list"],
        vec!["actor", "help"],
        vec!["actor", "list", "--help"],
    ] {
        let output = Command::new(mote_bin()).args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Usage:"), "{text}");
    }
}
