//! Coverage for `mote skills list` and `mote skills install`.

use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

#[test]
fn skills_list_emits_both_canonical_skills() {
    let out = Command::new(mote_bin())
        .args(["skills", "list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "list failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("mote-tracker"), "stdout:\n{stdout}");
    assert!(stdout.contains("mote-message-board"), "stdout:\n{stdout}");

    let json = Command::new(mote_bin())
        .args(["--json", "skills", "list"])
        .output()
        .unwrap();
    assert!(json.status.success());
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let arr = v.as_array().unwrap();
    let names: Vec<&str> = arr.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"mote-tracker"));
    assert!(names.contains(&"mote-message-board"));
    for skill in arr {
        let files = skill["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert!(
            paths.contains(&"SKILL.md"),
            "skill missing SKILL.md: {skill:?}"
        );
        assert!(
            paths.contains(&"agents/openai.yaml"),
            "skill missing agents/openai.yaml: {skill:?}"
        );
    }
}

#[test]
fn skills_install_requires_target_selection() {
    let out = Command::new(mote_bin())
        .args(["skills", "install"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--user") && stderr.contains("--repo"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn skills_install_repo_writes_both_agents_and_skills() {
    let td = TempDir::new().unwrap();
    let out = Command::new(mote_bin())
        .args(["skills", "install", "--repo"])
        .arg(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "install failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    for agent in ["claude", "codex"] {
        for skill in ["mote-tracker", "mote-message-board"] {
            let base = td
                .path()
                .join(format!(".{agent}"))
                .join("skills")
                .join(skill);
            let skill_md = base.join("SKILL.md");
            let openai_yaml = base.join("agents/openai.yaml");
            assert!(
                skill_md.is_file(),
                "missing SKILL.md for {agent}/{skill} at {}",
                skill_md.display()
            );
            assert!(
                openai_yaml.is_file(),
                "missing openai.yaml for {agent}/{skill} at {}",
                openai_yaml.display()
            );
            let s = fs::read_to_string(&skill_md).unwrap();
            assert!(
                s.contains(&format!("name: {skill}")),
                "frontmatter wrong for {agent}/{skill}:\n{s}"
            );
        }
    }
}

#[test]
fn skills_install_filters_by_agent_flag() {
    let td = TempDir::new().unwrap();
    let out = Command::new(mote_bin())
        .args(["skills", "install", "--agent", "claude", "--repo"])
        .arg(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "install failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        td.path()
            .join(".claude/skills/mote-tracker/SKILL.md")
            .is_file()
    );
    assert!(
        !td.path().join(".codex/skills").exists(),
        "codex skills should not be installed when --agent claude"
    );

    let bogus = Command::new(mote_bin())
        .args(["skills", "install", "--agent", "ghost", "--repo"])
        .arg(td.path())
        .output()
        .unwrap();
    assert_eq!(bogus.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&bogus.stderr);
    assert!(stderr.contains("unknown agent"), "stderr:\n{stderr}");
}

#[test]
fn skills_install_skips_existing_without_force_and_overwrites_with_force() {
    let td = TempDir::new().unwrap();
    let first = Command::new(mote_bin())
        .args(["skills", "install", "--agent", "claude", "--repo"])
        .arg(td.path())
        .output()
        .unwrap();
    assert!(first.status.success());

    let target = td.path().join(".claude/skills/mote-tracker/SKILL.md");
    fs::write(&target, "MUTATED LOCAL EDIT\n").unwrap();

    let second = Command::new(mote_bin())
        .args(["skills", "install", "--agent", "claude", "--repo"])
        .arg(td.path())
        .output()
        .unwrap();
    assert!(second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("skipped"),
        "second run should skip existing skill dirs:\n{stderr}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "MUTATED LOCAL EDIT\n");

    let forced = Command::new(mote_bin())
        .args([
            "skills", "install", "--agent", "claude", "--force", "--repo",
        ])
        .arg(td.path())
        .output()
        .unwrap();
    assert!(forced.status.success());
    let restored = fs::read_to_string(&target).unwrap();
    assert!(
        restored.contains("name: mote-tracker"),
        "forced install should restore canonical content:\n{restored}"
    );
}

#[test]
fn skills_install_does_not_follow_symlinks_when_overwriting() {
    let td = TempDir::new().unwrap();
    let canary_dir = td.path().join("canary");
    fs::create_dir(&canary_dir).unwrap();
    let canary_file = canary_dir.join("DO_NOT_DELETE.txt");
    fs::write(&canary_file, "sentinel").unwrap();

    let target_parent = td.path().join(".claude/skills");
    fs::create_dir_all(&target_parent).unwrap();
    let target = target_parent.join("mote-tracker");
    std::os::unix::fs::symlink(&canary_dir, &target).unwrap();

    let out = Command::new(mote_bin())
        .args([
            "skills", "install", "--agent", "claude", "--force", "--repo",
        ])
        .arg(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "forced install failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        canary_file.is_file(),
        "symlink target was destroyed; install should never follow symlinks during overwrite"
    );
    assert_eq!(fs::read_to_string(&canary_file).unwrap(), "sentinel");
    assert!(target.join("SKILL.md").is_file());
}
