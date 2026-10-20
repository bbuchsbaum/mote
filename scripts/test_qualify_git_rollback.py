#!/usr/bin/env python3
"""Exercise qualification cleanup with real owned children and bounded hooks."""
import json
import pathlib
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

import qualify_git_rollback as qualification


class HarnessCleanup(unittest.TestCase):
    def child(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(self.reap, child)
        return child

    @staticmethod
    def reap(child):
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)

    def test_release_failure_still_kills_and_waits_for_owned_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "authority.retire.entered").touch()
            child = self.child()
            with mock.patch.object(qualification, "release", side_effect=TimeoutError("injected gate failure")):
                errors = qualification.cleanup_owned(types.SimpleNamespace(root=root), [child], ["authority.retire"], set())
            self.assertEqual(child.returncode, -9)
            self.assertIn("injected gate failure", errors[0])
            self.assertEqual(json.loads((root / "cleanup-errors.json").read_text()), errors)

    def test_second_spawn_failure_cleans_first_registered_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            f = types.SimpleNamespace(root=pathlib.Path(temporary))
            child = self.child()
            with mock.patch.object(qualification, "migrated", return_value=(f, None, None, {})), \
                    mock.patch.object(qualification, "install_gate"), \
                    mock.patch.object(qualification, "start", side_effect=[child, OSError("injected spawn failure")]):
                with self.assertRaisesRegex(OSError, "spawn failure"):
                    qualification.race(f.root, None, "retirement")
            self.assertEqual(child.returncode, -9)

    def test_gate_wait_failure_cleans_both_registered_children(self):
        with tempfile.TemporaryDirectory() as temporary:
            f = types.SimpleNamespace(root=pathlib.Path(temporary))
            children = [self.child(), self.child()]
            with mock.patch.object(qualification, "migrated", return_value=(f, None, None, {})), \
                    mock.patch.object(qualification, "install_gate"), \
                    mock.patch.object(qualification, "start", side_effect=children), \
                    mock.patch.object(qualification, "wait_until", side_effect=TimeoutError("injected wait failure")):
                with self.assertRaisesRegex(TimeoutError, "wait failure"):
                    qualification.race(f.root, None, "retirement")
            self.assertEqual([child.returncode for child in children], [-9, -9])

    def test_unreleased_real_hook_expires(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            env = qualification.fixture_environment()

            def git(*args):
                return subprocess.run(["git", "-C", str(repository), *args], check=True,
                                      capture_output=True, env=env).stdout.decode().strip()

            git("init", "-q")
            git("config", "user.name", "qualification fixture")
            git("config", "user.email", "fixture@example.invalid")
            transactions = repository / "transactions"
            transactions.mkdir()
            (transactions / "00000000000000000001.json").write_text(json.dumps({
                "sequence": 1, "request": {"command": {"action": {"kind": "authority.retire"}}}}))
            git("add", "transactions")
            git("commit", "-qm", "synthetic gate fixture")
            commit = git("rev-parse", "HEAD")
            f = types.SimpleNamespace(root=root, remote=repository / ".git")
            qualification.install_gate(f, ["authority.retire"], timeout=0.15)
            started = time.monotonic()
            result = subprocess.run([str(f.remote / "hooks/pre-receive")],
                                    input=f"{'0' * 40} {commit} refs/heads/mote-state\n".encode(),
                                    capture_output=True, timeout=3, env=env)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertTrue((root / "authority.retire.entered").exists())
            self.assertLess(time.monotonic() - started, 3)


if __name__ == "__main__":
    unittest.main()
