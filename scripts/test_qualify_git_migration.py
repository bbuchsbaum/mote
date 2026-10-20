#!/usr/bin/env python3
"""Safety regression for the explicit disposable migration qualification."""
import hashlib
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("qualify_git_migration.py")


def files(root):
    return {p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob("*") if p.is_file()}


class SelectorIsolation(unittest.TestCase):
    def test_each_selector_refuses_without_touching_unrelated_repository(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            unrelated = root / "unrelated"
            env = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "MOTE_"))}
            env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
            subprocess.run(["git", "init", "-q", str(unrelated)], check=True, env=env)
            subprocess.run(["git", "-C", str(unrelated), "remote", "add", "origin", str(root / "sentinel.git")], check=True, env=env)
            (unrelated / "keep.txt").write_bytes(b"unrelated fixture bytes\0")
            before = files(unrelated)
            for name, value in [("GIT_DIR", str(unrelated / ".git")),
                                ("GIT_COMMON_DIR", str(unrelated / ".git")),
                                ("GIT_WORK_TREE", str(unrelated)), ("GIT_NAMESPACE", "fixture")]:
                output = root / ("output-" + name)
                result = subprocess.run([sys.executable, str(SCRIPT), "--binary", sys.executable,
                                         "--ordinary-binary", sys.executable, "--output", str(output)],
                                        cwd=unrelated, env={**env, name: value}, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"refuses inherited Git repository selectors", result.stderr)
                self.assertFalse(output.exists())
                self.assertEqual(files(unrelated), before)


if __name__ == "__main__":
    unittest.main()
