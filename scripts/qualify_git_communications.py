#!/usr/bin/env python3
"""Qualify migrated communications using one retained binary and local Git.

Creates only disposable fixtures beneath a new output directory. No hosted
remote, existing store or credentials are used; all evidence is retained.
"""
import argparse
import hashlib
import json
import os
import pathlib
import subprocess

from qualify_git_migration import Fixture, SELECTORS, fixture_environment
from qualify_git_rollback import cleanup_owned


def input_op(f, stem, kind, actor="alice", **fields):
    value = {"v": 1, "op": stem, "kind": kind, "actor": actor, "ts": "2026-10-20T00:00:00Z", **fields}
    (f.root / "a/.mote/ops" / (stem + ".json")).write_text(json.dumps(value, ensure_ascii=False))


def subject(result):
    assert result["status"] == "accepted", result
    return result["receipt"]["outcome"]["result"]["subject"]


def events(f, name, *args):
    raw = f.run(name, "--json", "events", *args, structured=False)
    return [json.loads(line) for line in raw.splitlines()]


def spawn(f, name, session, label, args):
    command, env = f.command(name, args, actor="bob", session=session)
    with (f.root / (label + ".stdout")).open("wb") as stdout, (f.root / (label + ".stderr")).open("wb") as stderr:
        return subprocess.Popen(command, cwd=f.root / name, env=env, stdout=stdout, stderr=stderr)


def retry_winning_ack(f, results):
    name, winner = next((name, result) for name, result in zip(("b", "c"), results)
                        if result["status"] == "accepted")
    head = f.head()
    repeated = f.run(name, "authority", "retry", winner["request_id"], actor="bob")
    assert repeated["receipt"] == winner["receipt"] and f.head() == head
    return name


def positive(root, binary):
    f = Fixture(root, binary)
    task = f.run("a", "new", "retained task", structured=False).decode().strip()
    f.run("a", "discuss", "topic", "new", "design")
    f.run("a", "discuss", "watch", "design", actor="bob")
    input_op(f, "b", "msg_send", msg_id="msg-雪", to="bob", entity=task,
             msg_kind="request", body="legacy request", idempotency_key="old-key")
    spaced_post = " post-with-spaces "
    input_op(f, "p", "board_post", post_id=spaced_post, topic="design", body="space-preserving reference")
    input_op(f, "文", "board_post", post_id="post-文", topic="design", body="retained Unicode post",
             ts="2099-01-01T00:00:00Z")
    input_op(f, "文z", "board_read", actor="bob", upto_op_id="文", topic="design")
    maximum = "雪" * 20
    input_op(f, maximum, "future_unknown")
    # Establish actual acceptance through the original local reader first.
    requests = f.run("a", "msg", "requests", actor="bob")
    assert any(m["msg_id"] == "msg-雪" for m in requests)
    before = f.ops("a")
    descriptor = f.run("a", "authority", "migrate-git", "--backup", str(root / "backup"))["descriptor"]
    assert "coordination-communications-v1" in descriptor["capabilities"]
    for name in ("b", "c"):
        f.run(name, "authority", "connect")
    legacy_events = events(f, "a")
    assert legacy_events and all(row["data"]["phase"] == "legacy" for row in legacy_events)
    old_cursor = legacy_events[-1]["data"]["cursor"]
    assert [row["data"] for row in events(f, "b")] == [row["data"] for row in legacy_events]
    alice, bob, other = f.start("a", "alice"), f.start("b", "bob"), f.start("c", "bob")
    posted = f.run("a", "discuss", "post", "--topic", "design", "new canonical post", session=alice)
    post = subject(posted)
    notifications = f.run("b", "discuss", "notifications", "--topic", "design", actor="bob")
    selected = notifications["data"]["posts"]
    assert [p["post_id"] for p in selected] == [post]
    assert selected[0]["sent_op_id"].startswith(maximum + "~gaop-")
    f.run("b", "discuss", "mark-read", "--topic", "design", "--through", post, actor="bob", session=bob)
    assert f.run("b", "discuss", "unread", "--topic", "design", actor="bob")["data"] == []
    reply = f.run("b", "msg", "reply", "msg-雪", "reviewed", actor="bob", session=bob)
    reply_id = subject(reply)
    thread = f.run("a", "msg", "thread", "bob")["data"]
    record = next(m for m in thread if m["msg_id"] == reply_id)
    assert record["entity"] == task and record["correlation_id"] == "msg-雪"
    f.run("a", "msg", "resolve", "msg-雪", session=alice)
    for body in ("legacy request", "changed content"):
        refused = f.run("a", "msg", "send", "--to", "bob", "--issue", task, "--kind", "request",
                        "--idempotency-key", "old-key", body, session=alice, failure=True)
        assert refused.returncode == 2
        assert b"old-key" in refused.stdout
    sent = f.run("a", "msg", "send", "--to", "bob", "ack race", session=alice)
    message = subject(sent)
    children = []
    try:
        children.append(spawn(f, "b", bob, "ack-b", ["msg", "ack", message]))
        children.append(spawn(f, "c", other, "ack-c", ["msg", "ack", message]))
        codes = [child.wait(timeout=30) for child in children]
        assert sorted(codes) == [0, 2], codes
        results = [json.loads((root / (label + ".stdout")).read_bytes()) for label in ("ack-b", "ack-c")]
    finally:
        cleanup_owned(f, children, [], set())
    winning_checkout = retry_winning_ack(f, results)
    promoted = f.run("b", "discuss", "promote", post, "--tag", "design", actor="bob", session=bob)
    new_task = subject(promoted)
    assert len(promoted["receipt"]["outcome"]["result"]["effect_ids"]) == 4
    assert len(f.run("a", "show", new_task)["data"]["notes"]) == 1
    decision = f.run("a", "discuss", "decide", "--topic", "design", "--agreed", spaced_post,
                     "--cite", "post:" + spaced_post, "--body", "preserve this imported citation", session=alice)
    decision_id = subject(decision)
    sequence = decision["receipt"]["sequence"]
    transaction = json.loads(f.git(root, "--git-dir", str(f.remote), "show",
                                   f"refs/heads/mote-state:transactions/{sequence:020}.json"))
    expected_reference = {"kind": "post", "post_id": spaced_post}
    assert transaction["request"]["command"]["action"]["action"]["references"] == [expected_reference]
    view = f.run("a", "authority", "refresh")["data"]["communications"]
    assert view["board_decisions"][decision_id]["references"] == [expected_reference]
    head = f.head()
    all_events = events(f, "a")
    tail = events(f, "b", "--after", old_cursor)
    assert tail and all(row["data"]["phase"] == "remote" for row in tail)
    assert [row["data"] for row in all_events] == [row["data"] for row in legacy_events + tail]
    positions = [(row["data"]["position"]["sequence"], row["data"]["position"]["effect_index"]) for row in tail]
    assert positions == sorted(set(positions))
    # Communication IDs sort above the Unicode seed; later task adapters sort
    # below them. Event order must still follow canonical effect positions.
    op_ids = [row["data"]["event"]["op_id"] for row in tail]
    assert any(left > right for left, right in zip(op_ids, op_ids[1:]))
    assert next(row for row in legacy_events if row["data"]["event"]["data"].get("post_id") == "post-文")["data"]["event"]["ts"].startswith("2099-")
    assert tail[0]["data"]["event"]["ts"] < "2099-"
    for command in (("actor", "status", "bob"), ("doctor",), ("fsck",), ("audit",)):
        observed = f.run("b", *command, actor="bob")
        assert observed["authority"]["fresh"] is True
    assert f.run("b", "actor", "status", "bob")["data"]["enrollment"] == "active"
    assert f.head() == head
    assert f.ops("a") == before
    return {"root": descriptor["root"], "exact_unicode_prefix": maximum, "ack_exit_codes": codes, "ack_retry_checkout": winning_checkout,
            "imported_reply_id": reply_id, "promotion_task": new_task, "original_ops_unchanged": True,
            "old_keys_preserved": True, "receipt_retry_does_not_append": True, "spaced_reference_round_trip": True,
            "legacy_remote_event_cursor_resume": True, "canonical_event_positions": positions,
            "backward_timestamps_and_lexical_ids_preserve_event_order": True, "diagnostics_publish_nothing": True}


def eligibility(root, binary, case):
    f = Fixture(root, binary)
    if case == "rejected-maximum":
        input_op(f, "z\x01", "future_unknown")
    else:
        stem = "a\x01" if case == "accepted-clock" else "a"
        post = "post-\x01" if case == "accepted-id" else "post-valid"
        input_op(f, stem, "board_post", actor=" alice " if case == "untrimmed-actor" else "alice",
                 post_id=post, topic="design", body="preserved", notify=["x" * 129] if case == "notification-actor" else [])
        input_op(f, "雪", "future_unknown")
        if case == "notification-actor":
            observed = next(row for row in f.run("a", "discuss", "list") if row["post_id"] == post)
            assert observed["explicit_notify"] == ["x" * 129]
            assert observed["notification_recipients"] == ["x" * 129]
    before, original_format = f.ops("a"), (root / "a/.mote/FORMAT.json").read_bytes()
    # Archival capture succeeds; operational refusal must be the profile gate.
    exported = f.run("a", "authority", "export-legacy", "--output", str(root / "archive"))
    refused = f.run("a", "authority", "migrate-git", "--backup", str(root / "backup"), failure=True)
    (root / "refusal.stdout").write_bytes(refused.stdout)
    (root / "refusal.stderr").write_bytes(refused.stderr)
    expected = b"legacy communication actor cannot be addressed" if case in ("untrimmed-actor", "notification-actor") else b"invalid communication reference"
    assert expected in refused.stderr, refused.stderr
    assert f.head() is None and f.ops("a") == before
    assert (root / "a/.mote/FORMAT.json").read_bytes() == original_format
    assert not (root / "a/.mote/local/git-setup.json").exists()
    assert not (root / "a/.mote/local/git-authority/binding.json").exists()
    assert not (root / "backup").exists()
    return {"case": case, "archive_digest": exported["digest"], "refused_before_publication_and_backup": True,
            "original_ops_and_format_unchanged": True}


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
              "eligibility": [eligibility(root / case, binary, case) for case in ("rejected-maximum", "accepted-id", "accepted-clock", "untrimmed-actor", "notification-actor")]}
    (root / "receipt.json").write_text(json.dumps(report, sort_keys=True, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
