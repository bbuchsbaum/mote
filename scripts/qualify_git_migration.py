#!/usr/bin/env python3
"""Qualify a retained development binary using disposable local Git fixtures.

This never opens the caller's Mote store, contacts hosted Git, or removes fixture
artifacts. Pass an unused output directory; all evidence stays there.
"""
import argparse
import hashlib
import json
import os
import pathlib
import shlex
import subprocess
import time


SELECTORS = ("GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE", "GIT_NAMESPACE")

def fixture_environment():
    # These fixtures use only local bare repositories. Global URL rewrites,
    # hooks, selectors and authentication configuration are not fixture inputs.
    env = {k: v for k, v in os.environ.items() if not k.startswith(("MOTE_", "GIT_"))}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0")
    return env


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def wait_until(predicate, seconds=20):
    deadline = time.monotonic() + seconds
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError("fixture condition timed out")
        time.sleep(0.02)


class Fixture:
    def __init__(self, root, binary):
        self.root, self.binary = root, binary
        root.mkdir(mode=0o700)
        self.remote = root / "remote.git"
        self.git(root, "init", "--bare", "-q", str(self.remote))
        self.environment = fixture_environment()
        self.temp = root / "scratch-owner"
        self.temp.mkdir(mode=0o700)
        self.environment["TMPDIR"] = str(self.temp)
        for name in ("a", "b", "c"):
            repo = root / name
            repo.mkdir()
            self.git(repo, "init", "-q")
            self.git(repo, "remote", "add", "origin", str(self.remote))
            self.run(name, "init", structured=False)

    @staticmethod
    def git(cwd, *args):
        return subprocess.run(["git", "-C", str(cwd), *args], check=True,
                              capture_output=True, env=fixture_environment()).stdout

    def command(self, name, args, actor="alice", session=None, binary=None, structured=True):
        env = self.environment.copy()
        if session:
            env["MOTE_SESSION"] = session
        command = [str(binary or self.binary), "--store", str(self.root / name / ".mote"),
                   "--actor", actor]
        if structured:
            command.append("--json")
        return command + list(args), env

    def run(self, name, *args, actor="alice", session=None, binary=None,
            structured=True, failure=False):
        command, env = self.command(name, args, actor, session, binary, structured)
        result = subprocess.run(command, cwd=self.root / name, env=env, capture_output=True)
        if failure:
            assert result.returncode != 0, args
            return result
        assert result.returncode == 0, (args, result.returncode, result.stderr.decode())
        return json.loads(result.stdout) if structured else result.stdout

    def ops(self, name):
        return {p.name: digest(p) for p in sorted((self.root / name / ".mote/ops").iterdir())}

    def head(self):
        out = subprocess.run(["git", "--git-dir", str(self.remote), "rev-parse", "--verify",
                              "refs/heads/mote-state"], capture_output=True, env=self.environment)
        return out.stdout.decode().strip() if out.returncode == 0 else None

    def start(self, name, actor):
        return self.run(name, "session", "start", "--as", actor, actor=actor)["session_id"]


def positive(root, binary, ordinary):
    f = Fixture(root, binary)
    task = f.run("a", "new", "retained title", structured=False).decode().strip()
    gone = f.run("a", "new", "deleted task", structured=False).decode().strip()
    f.run("a", "delete", gone, structured=False)
    f.run("a", "note", task, "--kind", "progress", "legacy note", structured=False)
    old = f.run("a", "show", task)
    private = root / "a/.mote/local/fixture-private.bin"
    private.write_bytes(b"synthetic private fixture\0\xff")
    (private.parent / "empty-fixture").mkdir()
    source = f.ops("a")
    (root / "a/dirty.txt").write_text("unrelated dirty source\n")
    fetch_head = root / "a/.git/FETCH_HEAD"
    fetch_head.write_bytes(b"preserve synthetic FETCH_HEAD\n")
    before_status = f.git(root / "a", "status", "--porcelain=v1", "-z")
    backup = root / "backup"
    migrated = f.run("a", "authority", "migrate-git", "--backup", str(backup))
    descriptor = migrated["descriptor"]
    assert f.head() == descriptor["root"]
    assert f.ops("a") == source
    assert f.git(root / "a", "status", "--porcelain=v1", "-z") == before_status
    assert fetch_head.read_bytes() == b"preserve synthetic FETCH_HEAD\n"
    assert (backup / "store/local/fixture-private.bin").read_bytes() == private.read_bytes()
    assert (backup / "store/local/empty-fixture").is_dir()
    assert backup.stat().st_mode & 0o077 == 0
    root_files = f.git(root, "--git-dir", str(f.remote), "ls-tree", "--name-only", descriptor["root"]).decode().splitlines()
    assert root_files == ["legacy.pack", "manifest.json"], root_files
    original_pack = f.git(root, "--git-dir", str(f.remote), "show", descriptor["root"] + ":legacy.pack")
    format_c = (root / "c/.mote/FORMAT.json").read_bytes()
    refused = f.run("c", "authority", "connect", binary=ordinary, failure=True)
    assert (root / "c/.mote/FORMAT.json").read_bytes() == format_c
    assert not (root / "c/.mote/remote.json").exists()
    cache_c = root / "c/.mote/local/git-authority"
    assert not (cache_c / "binding.json").exists()
    assert not (cache_c / "confirmed-head.json").exists()
    assert not (cache_c / "view.json").exists()
    assert b"legacy profile unavailable" in refused.stderr
    f.run("b", "authority", "connect")
    assert f.run("b", "show", task)["data"]["clock"]["title"] == old["clock"]["title"]
    assert any(n["text"] == "legacy note" for n in f.run("b", "show", task)["data"]["notes"])
    f.run("b", "show", gone, failure=True)
    a, b = f.start("a", "alice"), f.start("b", "bob")
    f.run("a", "set", task, "title=remote title", session=a)
    assert f.run("b", "show", task)["data"]["title"] == "remote title"
    history = f.run("b", "history", task, "--include-rejected")["data"]
    assert history[0]["phase"] == "legacy"
    assert any(row["phase"] == "remote" for row in history)
    assert [row["ordinal"] for row in history if row["phase"] == "legacy"] == sorted(row["ordinal"] for row in history if row["phase"] == "legacy")
    contenders = []
    for name, actor, session in (("a", "alice", a), ("b", "bob", b)):
        command, env = f.command(name, ["begin", task, "--paths", "src/shared"], actor, session)
        contenders.append(subprocess.Popen(command, cwd=root / name, env=env,
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE))
    outputs = [p.communicate(timeout=30) for p in contenders]
    codes = [p.returncode for p in contenders]
    assert sorted(codes) == [0, 2], (codes, outputs)
    winner = codes.index(0)
    name, actor, session = (("a", "alice", a), ("b", "bob", b))[winner]
    f.run(name, "done", task, "--note", "fixture complete", actor=actor, session=session)
    assert f.run("b", "show", task)["data"]["status"] == "closed"
    assert f.ops("a") == source
    assert f.git(root, "--git-dir", str(f.remote), "show", "refs/heads/mote-state:legacy.pack") == original_pack
    offline = root / "offline.git"
    f.remote.rename(offline)
    try:
        assert f.run("b", "authority", "cached")["authority"]["fresh"] is False
        f.run("b", "authority", "refresh", failure=True)
    finally:
        offline.rename(f.remote)
    confirmed_path = root / "b/.mote/local/git-authority/confirmed-head.json"
    confirmed_before = confirmed_path.read_bytes()
    f.git(root, "--git-dir", str(f.remote), "update-ref", "-d", "refs/heads/mote-state")
    f.run("b", "authority", "refresh", failure=True)
    f.run("b", "authority", "connect", failure=True)
    assert f.head() is None
    assert confirmed_path.read_bytes() == confirmed_before
    return {"root": descriptor["root"], "source_ops": len(source), "legacy_pack_sha256": hashlib.sha256(original_pack).hexdigest(),
            "race_exit_codes": codes, "unsupported_profile_exit": refused.returncode,
            "source_unchanged": True, "offline_cache_only": True, "backup_private_complete": True,
            "confirmed_missing_branch_not_recreated": True}


def interrupted(root, binary):
    f = Fixture(root, binary)
    task = f.run("a", "new", "interrupt fixture", structured=False).decode().strip()
    source = f.ops("a")
    fifo, entered = root / "release.fifo", root / "entered"
    os.mkfifo(fifo, 0o600)
    hook = f.remote / "hooks/pre-receive"
    hook.write_text("#!/bin/sh\nprintf ready > " + shlex.quote(str(entered)) + "\nread answer < " + shlex.quote(str(fifo)) + "\nexit 0\n")
    hook.chmod(0o700)
    command, env = f.command("a", ["authority", "migrate-git", "--backup", str(root / "backup")])
    with (root / "interrupted.stdout").open("wb") as stdout, (root / "interrupted.stderr").open("wb") as stderr:
        child = subprocess.Popen(command, cwd=root / "a", env=env, stdout=stdout, stderr=stderr)
        try:
            wait_until(entered.exists)
            setup_path = root / "a/.mote/local/git-setup.json"
            setup = json.loads(setup_path.read_bytes())
            expected = setup["descriptor"]["root"]
            child.kill()  # Only the exact fixture CLI process started above.
            child.wait(timeout=10)
            f.run("a", "new", "must remain fenced", failure=True)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)
            def release():
                try:
                    fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                except OSError:
                    return False
                with os.fdopen(fd, "wb") as stream:
                    stream.write(b"continue\n")
                return True
            wait_until(release)
    wait_until(lambda: f.head() == expected)
    hook.rename(f.remote / "hooks/pre-receive.completed")
    f.temp.rename(root / "retained-lost-scratch")
    f.temp.mkdir(mode=0o700)
    intent_before = setup_path.read_bytes()
    backup_format = root / "backup/store/FORMAT.json"
    backup_before = backup_format.read_bytes()
    backup_format.write_bytes(backup_before + b" ")
    f.run("a", "authority", "resume", failure=True)
    assert setup_path.read_bytes() == intent_before
    assert not (root / "a/.mote/local/git-authority/binding.json").exists()
    backup_format.write_bytes(backup_before)
    other = root / "other.git"
    f.git(root, "init", "--bare", "-q", str(other))
    f.git(root / "a", "remote", "set-url", "origin", str(other))
    f.run("a", "authority", "resume", failure=True)
    assert setup_path.read_bytes() == intent_before
    assert f.head() == expected
    assert f.git(root, "--git-dir", str(other), "for-each-ref", "--format=%(refname)", "refs/heads/mote-state") == b""
    f.git(root / "a", "remote", "set-url", "origin", str(f.remote))
    resumed = f.run("a", "authority", "resume")
    assert resumed["descriptor"]["root"] == expected
    assert f.head() == expected
    assert not setup_path.exists()
    assert f.ops("a") == source
    assert f.run("a", "show", task)["data"]["title"] == "interrupt fixture"
    return {"root": expected, "killed_pid": child.pid, "exit_code": child.returncode,
            "accepted_after_client_death": True, "scratch_loss_same_root": True,
            "source_unchanged": True, "damaged_backup_refused": True,
            "retargeted_endpoint_refused": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--ordinary-binary", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if any(name in os.environ for name in SELECTORS):
        raise SystemExit("qualification refuses inherited Git repository selectors")
    binary, ordinary = args.binary.resolve(strict=True), args.ordinary_binary.resolve(strict=True)
    output = args.output.absolute()
    output.mkdir(mode=0o700)
    profile = json.loads(subprocess.run([str(binary), "--json", "authority", "legacy-profile"], check=True, capture_output=True, env=fixture_environment()).stdout)
    report = {"binary": str(binary), "binary_sha256": digest(binary), "profile": profile,
              "positive": positive(output / "positive", binary, ordinary),
              "interrupted": interrupted(output / "interrupted", binary)}
    (output / "receipt.json").write_text(json.dumps(report, sort_keys=True, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
