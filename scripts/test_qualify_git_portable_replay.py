#!/usr/bin/env python3
"""Focused contract checks for portable replay qualification setup."""
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import qualify_git_portable_replay as qualification


class PortableReplayContract(unittest.TestCase):
    def test_parser_requires_both_independent_binaries_and_output(self):
        with self.assertRaises(SystemExit):
            qualification.parse_args([])
        args = qualification.parse_args(["--binary-a", "a", "--binary-b", "b", "--output", "out"])
        self.assertEqual((args.binary_a, args.binary_b, args.output),
                         (pathlib.Path("a"), pathlib.Path("b"), pathlib.Path("out")))

    def test_inherited_selector_refuses_before_output_is_created(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            output = root / "output"
            with mock.patch.dict(os.environ, {"GIT_DIR": str(root / "foreign.git")}, clear=False):
                with self.assertRaisesRegex(SystemExit, "inherited Git repository selectors"):
                    qualification.main(["--binary-a", __file__, "--binary-b", __file__, "--output", str(output)])
            self.assertFalse(output.exists())

    def test_existing_output_refuses_before_profile_or_fixture_work(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / "output"
            output.mkdir()
            with mock.patch.object(qualification, "legacy_profile") as profile:
                with self.assertRaisesRegex(SystemExit, "must not already exist"):
                    qualification.main(["--binary-a", __file__, "--binary-b", qualification.__file__, "--output", str(output)])
            profile.assert_not_called()

    def test_same_binary_bytes_are_rejected_before_profile_query(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / "output"
            with mock.patch.object(qualification, "legacy_profile") as profile:
                with self.assertRaisesRegex(SystemExit, "different SHA256"):
                    qualification.main(["--binary-a", __file__, "--binary-b", __file__, "--output", str(output)])
            profile.assert_not_called()

    def test_fixture_environment_scrubs_mote_and_git_inputs(self):
        with mock.patch.dict(os.environ, {"GIT_DIR": "foreign", "MOTE_STORE": "foreign-store"}, clear=False):
            environment = qualification.fixture_environment()
        self.assertNotIn("GIT_DIR", environment)
        self.assertNotIn("MOTE_STORE", environment)
        self.assertEqual(environment["GIT_CONFIG_NOSYSTEM"], "1")


if __name__ == "__main__":
    unittest.main()
