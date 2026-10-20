#!/usr/bin/env python3
"""Qualify pre-use rollback using a retained binary and disposable local Git.

All fixtures and logs are retained beneath a new output directory. No existing
project is opened, migrated, retired, restored or deleted.
"""
import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import time

from qualify_git_migration import Fixture, SELECTORS, fixture_environment, wait_until


def migrated(root, binary):
    f = Fixture(root, binary)
    task = f.run("a", "new", "rollback fixture", structured=False).decode().strip()
    before = f.ops("a")
    descriptor = f.run("a", "authority", "migrate-git", "--backup", str(root / "backup"))["descriptor"]
    assert "authority-retirement-v1" in descriptor["capabilities"]
    f.run("b", "authority", "connect")  # Connection is not a transaction.
    return f, task, before, descriptor


def rollback_args(output):
    return ["authority", "rollback-git", "--output", str(output), "--reason", "disposable qualification"]


def start(f, name, args, label, actor="alice"):
    command, env = f.command(name, args, actor=actor)
    stdout = (f.root / (label + ".stdout")).open("wb")
    stderr = (f.root / (label + ".stderr")).open("wb")
    try:
        child = subprocess.Popen(command, cwd=f.root / name, env=env, stdout=stdout, stderr=stderr)
    finally:
        stdout.close()
        stderr.close()
    return child


def finish(f, child, label, expected):
    code = child.wait(timeout=30)
    assert code == expected, (label, code, (f.root / (label + ".stderr")).read_text())
    return json.loads((f.root / (label + ".stdout")).read_bytes()) if expected == 0 else None


def install_gate(f, actions, timeout=20):
    for action in actions:
        os.mkfifo(f.root / (action + ".fifo"), 0o600)
    hook = f.remote / "hooks/pre-receive"
    configuration = {"root": str(f.root), "remote": str(f.remote), "actions": actions, "timeout": timeout}
    hook.write_text("#!" + sys.executable + "\n" +
                   "import json,os,pathlib,select,subprocess,sys,time\n" +
                   "c=json.loads(" + repr(json.dumps(configuration)) + ")\n" +
                   "line=sys.stdin.readline().split()\n" +
                   "new=line[1]\n" +
                   "files=subprocess.run(['git','--git-dir',c['remote'],'ls-tree','-r','--name-only',new],check=True,capture_output=True,text=True).stdout.splitlines()\n" +
                   "paths=sorted(p for p in files if p.startswith('transactions/'))\n" +
                   "tx=json.loads(subprocess.run(['git','--git-dir',c['remote'],'show',new+':'+paths[-1]],check=True,capture_output=True).stdout)\n" +
                   "action=tx['request']['command'].get('action',{}).get('kind','')\n" +
                   "if tx['sequence']==1 and action in c['actions']:\n" +
                   " p=pathlib.Path(c['root'])\n" +
                   " (p/(action+'.entered')).write_text(new)\n" +
                   " fd=os.open(p/(action+'.fifo'),os.O_RDONLY|os.O_NONBLOCK)\n" +
                   " deadline=time.monotonic()+c['timeout']; released=False\n" +
                   " while time.monotonic()<deadline:\n" +
                   "  ready,_,_=select.select([fd],[],[],0.1)\n" +
                   "  if ready:\n" +
                   "   if os.read(fd,1024): released=True; break\n" +
                   "   time.sleep(0.02)\n" +
                   " os.close(fd)\n" +
                   " if not released: sys.exit(1)\n")
    hook.chmod(0o700)


def release(f, action, timeout=5):
    def attempt():
        try:
            fd = os.open(f.root / (action + ".fifo"), os.O_WRONLY | os.O_NONBLOCK)
        except OSError:
            return False
        with os.fdopen(fd, "wb") as stream:
            stream.write(b"continue\n")
        return True
    wait_until(attempt, seconds=timeout)


def cleanup_owned(f, children, actions, released):
    errors = []
    try:
        for action in actions:
            if action not in released and (f.root / (action + ".entered")).exists():
                try:
                    release(f, action, timeout=1)
                except Exception as error:
                    errors.append("gate release: " + str(error))
    finally:
        for child in children:
            try:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=10)
            except Exception as error:
                errors.append("exact child cleanup: " + str(error))
    if errors:
        (f.root / "cleanup-errors.json").write_text(json.dumps(errors) + "\n")
    return errors


def new_output(root):
    parent = root / "restored-local"
    parent.mkdir(mode=0o700)
    return parent / ".mote"


def run_local(f, command, label):
    result = subprocess.run(command, cwd=f.root, env=fixture_environment(), capture_output=True)
    (f.root / (label + ".stdout")).write_bytes(result.stdout)
    (f.root / (label + ".stderr")).write_bytes(result.stderr)
    assert result.returncode == 0, (label, result.returncode, result.stderr.decode())
    return result


def positive(root, binary):
    f, task, before, descriptor = migrated(root, binary)
    output = new_output(root)
    result = f.run("a", *rollback_args(output))
    assert result["retirement"]["sequence"] == 1
    assert result["retirement"]["outcome"]["status"] == "accepted"
    assert result["previously_completed"] is False
    assert f.ops("a") == before
    refused = f.run("b", "session", "start", "--as", "bob", actor="bob", failure=True)
    assert refused.returncode == 2
    assert b"authority_retired" in refused.stdout
    assert f.run("b", "authority", "refresh")["data"]["retirement"]["request_id"] == result["request_id"]
    original_format = (root / "backup/store/FORMAT.json").read_bytes()
    assert (output / "FORMAT.json").read_bytes() == original_format
    assert output.stat().st_mode & 0o077 == 0
    new = run_local(f, [str(binary), "--store", str(output), "--actor", "alice", "new", "survives completed retry"], "restored-new").stdout.decode().strip()
    after = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in (output / "ops").iterdir()}
    f.git(root / "a", "remote", "remove", "origin")
    repeated = f.run("a", *rollback_args(output))
    assert repeated["previously_completed"] is True
    assert repeated["request_id"] == result["request_id"]
    assert after == {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in (output / "ops").iterdir()}
    show = run_local(f, [str(binary), "--store", str(output), "--json", "show", new], "restored-show")
    assert json.loads(show.stdout)["title"] == "survives completed retry"
    assert any(json.loads(p.read_bytes()).get("entity") == task for p in (output / "ops").iterdir())
    return {"root": descriptor["root"], "request_id": result["request_id"], "retirement_commit": result["retirement"]["commit"],
            "completed_retry_without_git_alias": True, "later_local_work_preserved": True, "original_ops_unchanged": True}


def existing_destination(root, binary):
    f, _, _, descriptor = migrated(root, binary)
    incompatible = root / "incompatible-store-name"
    f.run("a", *rollback_args(incompatible), failure=True)
    assert f.head() == descriptor["root"]
    assert not (root / "a/.mote/local/git-rollback.json").exists()
    assert not incompatible.exists()
    parent = root / "foreign"
    parent.mkdir(mode=0o700)
    output = parent / ".mote"
    output.mkdir(mode=0o700)
    (output / "keep").write_bytes(b"foreign fixture bytes")
    f.run("a", *rollback_args(output), failure=True)
    assert f.head() == descriptor["root"]
    assert (output / "keep").read_bytes() == b"foreign fixture bytes"
    assert not (root / "a/.mote/local/git-rollback.json").exists()
    return {"refused_before_retirement": True, "foreign_bytes_unchanged": True}


def race(root, binary, winner):
    f, _, _, descriptor = migrated(root, binary)
    actions = ["authority.retire", "session.start"]
    install_gate(f, actions)
    output = new_output(root)
    children = []
    released = set()
    try:
        retire = start(f, "a", rollback_args(output), "retire")
        children.append(retire)
        enroll = start(f, "b", ["session", "start", "--as", "bob"], "enroll", "bob")
        children.append(enroll)
        for action in actions:
            wait_until((root / (action + ".entered")).exists)
        chosen = "authority.retire" if winner == "retirement" else "session.start"
        other = next(action for action in actions if action != chosen)
        release(f, chosen)
        released.add(chosen)
        wait_until(lambda: f.head() != descriptor["root"])
        release(f, other)
        released.add(other)
        retired = finish(f, retire, "retire", 0 if winner == "retirement" else 2)
        enrolled = finish(f, enroll, "enroll", 2 if winner == "retirement" else 0)
        if winner == "retirement":
            assert output.joinpath("FORMAT.json").exists()
            assert retired["retirement"]["sequence"] == 1
            assert b"authority_retired" in (root / "enroll.stdout").read_bytes()
        else:
            assert not output.exists()
            assert f.run("b", "authority", "refresh")["data"].get("retirement") is None
            f.run("b", "new", "work after rollback lost race", actor="bob", session=enrolled["session_id"])
        return {"forced_winner": winner, "retire_exit": retire.returncode, "enroll_exit": enroll.returncode,
                "restoration_only_if_retirement_wins": True, "both_prepared_root": descriptor["root"]}
    finally:
        cleanup_owned(f, children, actions, released)


def ambiguous_retirement(root, binary):
    f, _, before, descriptor = migrated(root, binary)
    install_gate(f, ["authority.retire"])
    output = new_output(root)
    children = []
    released = set()
    try:
        child = start(f, "a", rollback_args(output), "interrupted-retire")
        children.append(child)
        wait_until((root / "authority.retire.entered").exists)
        journal = json.loads((root / "a/.mote/local/git-rollback.json").read_bytes())
        request_id = journal["request"]["request_id"]
        child.kill()
        child.wait(timeout=10)
        release(f, "authority.retire")
        released.add("authority.retire")
        wait_until(lambda: f.head() != descriptor["root"])
        accepted = f.head()
        assert not output.exists()
        f.temp.rename(root / "retained-lost-scratch")
        f.temp.mkdir(mode=0o700)
        result = f.run("a", *rollback_args(output))
        assert result["request_id"] == request_id
        assert result["retirement"]["commit"] == accepted
        assert result["retirement"]["sequence"] == 1
        assert f.ops("a") == before
        assert f.head() == accepted  # Receipt recovery did not append another tx.
        return {"request_id": request_id, "accepted_after_client_death": True, "retirement_commit": accepted,
                "same_receipt_after_scratch_loss": True, "source_ops_unchanged": True}
    finally:
        cleanup_owned(f, children, ["authority.retire"], released)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if any(name in os.environ for name in SELECTORS):
        raise SystemExit("qualification refuses inherited Git repository selectors")
    binary = args.binary.resolve(strict=True)
    root = args.output.absolute()
    root.mkdir(mode=0o700)
    profile = json.loads(subprocess.run([str(binary), "--json", "authority", "legacy-profile"], check=True,
                                       capture_output=True, env=fixture_environment()).stdout)
    report = {"binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "profile": profile,
              "positive": positive(root / "positive", binary),
              "existing_destination": existing_destination(root / "existing", binary),
              "retirement_wins": race(root / "retirement-wins", binary, "retirement"),
              "first_use_wins": race(root / "first-use-wins", binary, "enrollment"),
              "ambiguous_retirement": ambiguous_retirement(root / "ambiguous", binary)}
    (root / "receipt.json").write_text(json.dumps(report, sort_keys=True, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
