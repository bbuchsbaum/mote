#!/usr/bin/env python3
"""Regression for retrying a race winner from its owning checkout journal."""
import unittest

from qualify_git_communications import retry_winning_ack


class ReceiptOwnership(unittest.TestCase):
    def test_c_winner_retries_c_journal(self):
        results = [{"status": "rejected", "request_id": "from-b", "receipt": {"sequence": 9}},
                   {"status": "accepted", "request_id": "from-c", "receipt": {"sequence": 8}}]

        class Journals:
            calls = []

            @staticmethod
            def head():
                return "unchanged-canonical-head"

            def run(self, checkout, *args, actor):
                self.calls.append(checkout)
                # Retained intent exists only in the submitting checkout.
                journal = {"b": {"from-b": results[0]}, "c": {"from-c": results[1]}}
                return journal[checkout][args[-1]]

        fixture = Journals()
        self.assertEqual(retry_winning_ack(fixture, results), "c")
        self.assertEqual(fixture.calls, ["c"])


if __name__ == "__main__":
    unittest.main()
