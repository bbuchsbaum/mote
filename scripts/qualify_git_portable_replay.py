#!/usr/bin/env python3
"""Qualify portable Git-authority replay with two independently built binaries.

The script creates a disposable, local-only fixture below a new output
directory.  It never reads the caller's Mote store or contacts a hosted Git
remote.  The two supplied executables must differ byte-for-byte while exposing
the same portable legacy-replay profile.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess

from qualify_git_migration import Fixture, SELECTORS, fixture_environment


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def binary_identity(binary):
    result = subprocess.run([str(binary), "--json", "authority", "legacy-profile"],
                            check=True, capture_output=True, env=fixture_environment())
    return {"path": str(binary), "sha256": digest(binary), "profile": json.loads(result.stdout)}


def legacy_profile(binary):
    completed = subprocess.run(
        [str(binary), "--json", "authority", "legacy-profile"], check=True,
        capture_output=True, env=fixture_environment(),
    )
    profile = json.loads(completed.stdout)
    if profile.get("schema") != "mote.legacy-profile.v2":
        raise AssertionError("portable qualification requires mote.legacy-profile.v2")
    profile_id = profile.get("profile_id")
    if profile.get("compatibility") != "same-source-locked-dependencies-v2":
        raise AssertionError("portable qualification requires same-source-locked-dependencies-v2")
    if not isinstance(profile_id, str) or not profile_id.startswith("portable-v2:"):
        raise AssertionError("portable qualification requires a portable-v2 profile_id")
    build = profile.get("build")
    if not isinstance(build, dict) or set(build) != {
        "input_digest", "build_config_digest", "executable_digest", "id", "portable_id"
    } or build["portable_id"] != profile_id:
        raise AssertionError("portable qualification requires complete exact-build provenance")
    return profile


def subject(result):
    if result.get("status") == "accepted":
        return result["receipt"]["outcome"]["result"]["subject"]
    # Before migration, local message publication returns the durable message
    # record rather than a remote receipt envelope.
    assert result.get("accepted") is True and "msg_id" in result, result
    return result["msg_id"]


def event_data(fixture, name, binary):
    raw = fixture.run(name, "--json", "events", structured=False, binary=binary)
    return [json.loads(line)["data"] for line in raw.splitlines()]


def unpack_legacy_pack(packed):
    remaining = memoryview(packed)
    magic = b"MOTE-LEGACY-PACK\0\x01"
    assert remaining[:len(magic)].tobytes() == magic
    remaining = remaining[len(magic):]
    count = int.from_bytes(remaining[:8], "big")
    remaining = remaining[8:]
    files = {}
    for _ in range(count):
        name_size = int.from_bytes(remaining[:4], "big")
        remaining = remaining[4:]
        name = remaining[:name_size].tobytes().decode()
        remaining = remaining[name_size:]
        payload_size = int.from_bytes(remaining[:8], "big")
        remaining = remaining[8:]
        files[name] = remaining[:payload_size].tobytes()
        remaining = remaining[payload_size:]
    assert not remaining
    return files


def imported_fixture(fixture, migrator):
    """Create state which exercises tasks, history, messages, and cursors."""
    task = fixture.run("a", "new", "portable imported task", structured=False,
                       binary=migrator).decode().strip()
    fixture.run("a", "note", task, "--kind", "progress", "portable legacy note",
                structured=False, binary=migrator)
    fixture.run("a", "discuss", "topic", "new", "portable", binary=migrator)
    fixture.run("a", "discuss", "watch", "portable", actor="bob", binary=migrator)
    post = fixture.run("a", "discuss", "post", "--topic", "portable", "portable archived post",
                       binary=migrator)
    post_id = post["post_id"]
    fixture.run("a", "discuss", "decide", "--topic", "portable", "--agreed", post_id,
                "--body", "portable archived decision", binary=migrator)
    message = fixture.run("a", "msg", "send", "--to", "bob", "--issue", task,
                          "--kind", "request", "portable imported message", binary=migrator)
    message_id = subject(message)
    source_ops = fixture.ops("a")
    descriptor = fixture.run("a", "authority", "migrate-git", "--backup",
                             str(fixture.root / "backup"), binary=migrator)["descriptor"]
    return task, message_id, source_ops, descriptor


def assert_imported(fixture, task, message_id, source_ops, migrator, client):
    fixture.run("b", "authority", "connect", binary=client)
    shown = fixture.run("b", "show", task, binary=client)["data"]
    assert shown["title"] == "portable imported task"
    assert any(note["text"] == "portable legacy note" for note in shown["notes"])
    assert fixture.ops("a") == source_ops
    thread = fixture.run("b", "msg", "thread", "bob", binary=client)["data"]
    assert any(row["msg_id"] == message_id and row["body"] == "portable imported message" for row in thread)
    decisions = fixture.run("b", "discuss", "decide", "--topic", "portable", "--show", binary=client)
    assert len(decisions["data"]["decisions"]) == 1
    assert decisions["data"]["decisions"][0]["post"]["body"] == "portable archived decision"
    remote_history = fixture.run("a", "history", task, "--include-rejected", binary=migrator)["data"]
    assert fixture.run("b", "history", task, "--include-rejected", binary=client)["data"] == remote_history
    remote_events = event_data(fixture, "a", migrator)
    replayed_events = event_data(fixture, "b", client)
    assert replayed_events == remote_events
    assert replayed_events[-1]["cursor"] == remote_events[-1]["cursor"]


def publish_and_replay(fixture, task, publisher_name, publisher_binary, reader_name, reader_binary,
                       actor, session, title, message):
    changed = fixture.run(publisher_name, "set", task, "title=" + title, actor=actor,
                          session=session, binary=publisher_binary)
    assert changed["status"] == "accepted"
    recipient = "alice" if actor == "bob" else "bob"
    sent = fixture.run(publisher_name, "msg", "send", "--to", recipient,
                       "--issue", task, "--kind", "request", message, actor=actor, session=session,
                       binary=publisher_binary)
    message_id = subject(sent)
    fixture.run(reader_name, "authority", "refresh", binary=reader_binary)
    assert fixture.run(reader_name, "show", task, binary=reader_binary)["data"]["title"] == title
    thread = fixture.run(reader_name, "msg", "thread", actor, actor=recipient,
                         binary=reader_binary)["data"]
    assert any(row["msg_id"] == message_id and row["body"] == message for row in thread)
    return changed


def replay_direction(root, migrator, client):
    fixture = Fixture(root, migrator)
    task, message_id, source_ops, descriptor = imported_fixture(fixture, migrator)
    assert_imported(fixture, task, message_id, source_ops, migrator, client)
    alice = fixture.run("a", "session", "start", "--as", "alice", actor="alice", binary=migrator)["session_id"]
    bob = fixture.run("b", "session", "start", "--as", "bob", actor="bob", binary=client)["session_id"]
    first = publish_and_replay(fixture, task, "a", migrator, "b", client, "alice", alice,
                               "published by migrator", "communication by migrator")
    second = publish_and_replay(fixture, task, "b", client, "a", migrator, "bob", bob,
                                "published by client", "communication by client")
    head = fixture.head()
    retried = fixture.run("b", "authority", "retry", second["request_id"], actor="bob", binary=migrator)
    assert retried["receipt"] == second["receipt"]
    assert fixture.head() == head
    return {
        "root": descriptor["root"],
        "final_head": head,
        "imported_task": task,
        "imported_message": message_id,
        "migrator_receipt": first["receipt"],
        "client_receipt": second["receipt"],
        "cross_build_retry_receipt": retried["receipt"],
        "cross_build_retry_head": head,
        "original_ops_unchanged": fixture.ops("a") == source_ops,
        "all_imported_state_replayed": True,
    }


def cross_build_continuation(root, binary_a, binary_b):
    fixture = Fixture(root, binary_a)
    task = fixture.run("a", "new", "cross-build ownership", structured=False).decode().strip()
    fixture.run("a", "discuss", "topic", "new", "portable")
    source = fixture.ops("a")
    descriptor = fixture.run("a", "authority", "migrate-git", "--backup", str(root / "backup"))["descriptor"]
    fixture.run("b", "authority", "connect", binary=binary_b)
    alice = fixture.run("a", "session", "start", "--as", "alice")["session_id"]
    bob = fixture.run("b", "session", "start", "--as", "bob", actor="bob", binary=binary_b)["session_id"]
    begun = fixture.run("a", "begin", task, "--note", "portable ownership", "--announce", "portable",
                        "--paths", "src/shared.rs", session=alice)
    assert begun["status"] == "accepted"
    result = begun["receipt"]["outcome"]["result"]
    assert len(result["effect_ids"]) == 5
    reservation, announcement = result["reservation"], result["announcement"]["id"]
    view = fixture.run("b", "authority", "refresh", binary=binary_b)["data"]
    assert view == fixture.run("a", "authority", "refresh")["data"]
    assert view["claims"][task]["token"] == result["claim_token"]
    assert view["claims"][task]["session"] == alice
    assert view["reservations"][reservation["id"]]["token"] == reservation["token"]
    assert view["reservations"][reservation["id"]]["paths"] == ["src/shared.rs"]
    post = view["communications"]["board_posts"][announcement]
    assert post["body"] == f"claiming {task} for {reservation['id']} on src/shared.rs"
    failed = fixture.run("b", "begin", task, "--paths", "src/shared.rs", actor="bob",
                         session=bob, binary=binary_b, failure=True)
    assert failed.returncode == 2
    rejected = json.loads(failed.stdout)
    assert rejected["status"] == "rejected"
    sequence = rejected["receipt"]["sequence"]
    tx = json.loads(fixture.git(root, "--git-dir", str(fixture.remote), "show",
                               f"refs/heads/mote-state:transactions/{sequence:020}.json"))
    assert tx["effects"] == [] and tx["outcome"]["status"] == "rejected"
    assert fixture.run("a", "authority", "refresh")["data"] == view
    assert fixture.run("a", "authority", "receipt", rejected["request_id"])["data"]["receipt"] == rejected["receipt"]
    read = fixture.run("b", "discuss", "mark-read", "--topic", "portable", "--through", announcement,
                       actor="bob", session=bob, binary=binary_b)
    assert read["status"] == "accepted"
    cursor = {"actor": "bob", "topic": "portable", "op_id": post["sent_op_id"]}
    assert cursor in fixture.run("a", "authority", "refresh")["data"]["communications"]["board_topic_read_cursors"]
    head = fixture.head()
    retried = fixture.run("b", "authority", "retry", rejected["request_id"], actor="bob", binary=binary_a, failure=True)
    assert retried.returncode == 2 and json.loads(retried.stdout)["receipt"] == rejected["receipt"]
    assert fixture.head() == head
    handed = fixture.run("a", "handoff", task, "--to", "bob", "--release", session=alice)
    assert handed["status"] == "accepted"
    completed = fixture.run("b", "done", task, "--note", "portable completion", actor="bob", session=bob, binary=binary_b)
    assert completed["status"] == "accepted"
    final = fixture.run("a", "authority", "refresh")["data"]
    assert final == fixture.run("b", "authority", "refresh", binary=binary_b)["data"]
    assert final["tasks"][task]["status"] == "closed" and task not in final["claims"]
    assert final["reservations"][reservation["id"]]["status"] == "closed"
    assert fixture.ops("a") == source
    return {"root": descriptor["root"], "head": fixture.head(), "task": task,
            "binaries": [binary_identity(binary_a), binary_identity(binary_b)],
            "begin_receipt": begun["receipt"], "rejected_receipt": rejected["receipt"],
            "read_receipt": read["receipt"], "cursor": cursor,
            "handoff_receipt": handed["receipt"], "done_receipt": completed["receipt"],
            "complete_views_equal": True, "original_ops_unchanged": True}


def unknown_original_operation(root, binary):
    fixture = Fixture(root, binary)
    task = fixture.run("a", "new", "unknown-op history", structured=False, binary=binary).decode().strip()
    (root / "a/.mote/ops/future-original.json").write_text(json.dumps({
        "v": 1, "op": "future-original", "kind": "future_unknown", "actor": "alice",
        "ts": "2026-10-20T00:00:00Z", "entity": task,
    }))
    descriptor = fixture.run("a", "authority", "migrate-git", "--backup", str(root / "backup"),
                             binary=binary)["descriptor"]
    fixture.run("b", "authority", "connect", binary=binary)
    pack = fixture.git(root, "--git-dir", str(fixture.remote), "show", descriptor["root"] + ":legacy.pack")
    decisions = json.loads(unpack_legacy_pack(pack)["legacy/decisions.json"])
    rejected = [event for row in decisions["inputs"] for event in row["orphan_events"]
                if event.get("op_id") == "future-original"]
    assert rejected and rejected[0]["accepted"] is False, decisions
    return {"root": descriptor["root"], "unknown_op_id": "future-original",
            "preserved_as_rejected_imported_history": True}


def incompatible_profile(root, binary, incompatible):
    identity = binary_identity(incompatible)
    assert identity["profile"]["profile_id"] != legacy_profile(binary)["profile_id"]
    fixture = Fixture(root, binary)
    task, _, _, descriptor = imported_fixture(fixture, binary)
    before_format = (root / "b/.mote/FORMAT.json").read_bytes()
    refused = fixture.run("b", "authority", "connect", binary=incompatible, failure=True)
    assert b"unsupported legacy replay profile" in refused.stderr
    assert fixture.head() == descriptor["root"]
    assert (root / "b/.mote/FORMAT.json").read_bytes() == before_format
    cache = root / "b/.mote/local/git-authority"
    assert not (cache / "binding.json").exists()
    assert not (cache / "confirmed-head.json").exists()
    assert not (cache / "view.json").exists()
    assert fixture.run("a", "show", task, binary=binary)["data"]["title"] == "portable imported task"
    return {"binary": identity, "root": descriptor["root"], "exit_code": refused.returncode, "stderr": refused.stderr.decode(errors="replace"),
            "rejected_before_activation": True,
            "rejected_before_highwater": True}


def exact_v1_root(root, exact_binary, portable_binary):
    """A new portable reader must not silently accept an exact-v1 archive."""
    identity = binary_identity(exact_binary)
    assert identity["profile"]["schema"] == "mote.legacy-profile.v1"
    fixture = Fixture(root, exact_binary)
    task = fixture.run("a", "new", "exact v1 archive", structured=False, binary=exact_binary).decode().strip()
    descriptor = fixture.run("a", "authority", "migrate-git", "--backup", str(root / "backup"),
                             binary=exact_binary)["descriptor"]
    fixture.run("b", "authority", "connect", binary=exact_binary)
    assert fixture.run("b", "show", task, binary=exact_binary)["data"]["title"] == "exact v1 archive"
    before_format = (root / "c/.mote/FORMAT.json").read_bytes()
    before_head = fixture.head()
    refused = fixture.run("c", "authority", "connect", binary=portable_binary, failure=True)
    assert b"unsupported legacy replay profile" in refused.stderr
    assert fixture.head() == before_head == descriptor["root"]
    assert (root / "c/.mote/FORMAT.json").read_bytes() == before_format
    cache = root / "c/.mote/local/git-authority"
    assert not (cache / "binding.json").exists()
    assert not (cache / "confirmed-head.json").exists()
    assert not (cache / "view.json").exists()
    return {"binary": identity, "root": descriptor["root"], "exit_code": refused.returncode,
            "stderr": refused.stderr.decode(errors="replace"),
            "original_exact_v1_reader_succeeds": True, "portable_reader_refused_before_confirmation": True}


def tampered_archive(root, binary):
    fixture = Fixture(root, binary)
    _, _, _, descriptor = imported_fixture(fixture, binary)
    tamper = root / "tamper"
    tamper.mkdir()
    fixture.git(tamper, "init", "-q")
    fixture.git(tamper, "remote", "add", "origin", str(fixture.remote))
    fixture.git(tamper, "fetch", "-q", "origin", "mote-state")
    fixture.git(tamper, "checkout", "-q", "FETCH_HEAD")
    pack = tamper / "legacy.pack"
    pack.write_bytes(pack.read_bytes() + b"tampered portable qualification pack")
    fixture.git(tamper, "add", "legacy.pack")
    fixture.git(tamper, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                "commit", "-qm", "tamper retained archive")
    fixture.git(tamper, "push", "-q", "origin", "HEAD:refs/heads/mote-state")
    before_format = (root / "c/.mote/FORMAT.json").read_bytes()
    refused = fixture.run("c", "authority", "connect", binary=binary, failure=True)
    assert (root / "c/.mote/FORMAT.json").read_bytes() == before_format
    cache = root / "c/.mote/local/git-authority"
    assert not (cache / "binding.json").exists()
    assert not (cache / "confirmed-head.json").exists()
    assert not (cache / "view.json").exists()
    assert b"authority append changed history" in refused.stderr
    return {"original_root": descriptor["root"], "tampered_head": fixture.head(),
            "exit_code": refused.returncode, "stderr": refused.stderr.decode(errors="replace"),
            "immutable_archive_append_refused_before_confirmation": True}


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-a", type=pathlib.Path, required=True)
    parser.add_argument("--binary-b", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--incompatible-binary", type=pathlib.Path)
    parser.add_argument("--exact-v1-binary", type=pathlib.Path)
    return parser.parse_args(argv)


def write_progress(output, report):
    (output / "progress.json").write_text(json.dumps(report, sort_keys=True, indent=2) + "\n")


def main(argv=None):
    args = parse_args(argv)
    if any(name in __import__("os").environ for name in SELECTORS):
        raise SystemExit("qualification refuses inherited Git repository selectors")
    binary_a = args.binary_a.resolve(strict=True)
    binary_b = args.binary_b.resolve(strict=True)
    incompatible = args.incompatible_binary.resolve(strict=True) if args.incompatible_binary else None
    exact_v1 = args.exact_v1_binary.resolve(strict=True) if args.exact_v1_binary else None
    output = args.output.absolute()
    if output.exists():
        raise SystemExit("qualification output must not already exist")
    sha_a, sha_b = digest(binary_a), digest(binary_b)
    if sha_a == sha_b:
        raise SystemExit("--binary-a and --binary-b must have different SHA256 values")
    profile_a, profile_b = legacy_profile(binary_a), legacy_profile(binary_b)
    if profile_a["profile_id"] != profile_b["profile_id"]:
        raise SystemExit("independent binaries do not share a portable profile_id")
    output.mkdir(mode=0o700)
    report = {
        "binary_a": str(binary_a), "binary_a_sha256": sha_a,
        "binary_b": str(binary_b), "binary_b_sha256": sha_b,
        "profile_id": profile_a["profile_id"], "profiles": {"a": profile_a, "b": profile_b},
    }
    write_progress(output, report)
    report["a_migrates_b_replays"] = replay_direction(output / "a-migrates-b-replays", binary_a, binary_b)
    write_progress(output, report)
    report["b_migrates_a_replays"] = replay_direction(output / "b-migrates-a-replays", binary_b, binary_a)
    write_progress(output, report)
    report["cross_build_continuation"] = cross_build_continuation(output / "cross-build-continuation", binary_a, binary_b)
    write_progress(output, report)
    report["unknown_original_operation"] = unknown_original_operation(output / "unknown-original", binary_a)
    write_progress(output, report)
    report["tampered_archive"] = tampered_archive(output / "tampered-archive", binary_a)
    write_progress(output, report)
    if incompatible:
        report["incompatible_profile"] = incompatible_profile(output / "incompatible-profile", binary_a, incompatible)
        write_progress(output, report)
    if exact_v1:
        report["exact_v1_root"] = exact_v1_root(output / "exact-v1-root", exact_v1, binary_a)
    (output / "receipt.json").write_text(json.dumps(report, sort_keys=True, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
