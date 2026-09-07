"""Integration tests against real fixture/client processes; retain failure evidence on disk."""

import argparse
import json
from pathlib import Path
import subprocess
import unittest

import run


class RunnerTests(unittest.TestCase):
    def check_cleanup(self, folder):
        children = json.loads((folder / "cleanup.json").read_text())
        self.assertEqual(len(children), 2)
        for child in children:
            self.assertTrue(child.get("joined"), child)
            self.assertIsNotNone(child.get("exit_code"), child)

    def test_corrupted_payload_invalidates_run_and_joins_both_children(self):
        folder = OPTIONS.output / "corrupt"
        with self.assertRaises(EOFError):
            run.run_case(OPTIONS.plain, OPTIONS.meter, folder, "test-corrupt", "https", 1,
                         1024, 1, corrupt=True)
        self.assertIn("response bytes differ", (folder / "client.stderr").read_text())
        self.assertFalse((folder / "result.json").exists())
        self.check_cleanup(folder)

    def test_timeout_invalidates_run_and_joins_both_children(self):
        folder = OPTIONS.output / "timeout"
        with self.assertRaises(TimeoutError):
            run.run_case(OPTIONS.plain, OPTIONS.plain, folder, "test-timeout", "http", 1,
                         1024, 1, timeout=0.4)
        self.assertFalse((folder / "result.json").exists())
        self.check_cleanup(folder)

    def test_success_requires_exact_phases_reuse_concurrency_and_shutdown(self):
        folder = OPTIONS.output / "success"
        result = run.run_case(OPTIONS.plain, OPTIONS.meter, folder, "test-success", "https", 16,
                              8192, 2)
        self.assertTrue(result["valid"])
        self.assertEqual(result["fixture_stats"]["high_active_requests"], 16)
        self.assertEqual(result["final"]["metrics"]["accepted"], 64)
        self.assertEqual(result["final"]["metrics"]["reused"], 48)
        for phase in result["phases"]:
            self.assertGreaterEqual(phase["allocation"]["peak_live"], phase["allocation"]["live_after"])
            self.assertGreaterEqual(phase["process"]["samples"], 2)
        self.check_cleanup(folder)

    def test_system_trust_still_works_with_additional_private_root(self):
        # Public networking is deliberately separate from the loopback benchmark timings.
        root = OPTIONS.output / "system-root.der"
        fixture = run.Child([OPTIONS.plain, "fixture"], OPTIONS.output / "system-fixture.stderr")
        try:
            ready = fixture.receive(10)
            root.write_bytes(bytes(ready["root_der"]))
        finally:
            self.assertTrue(fixture.close()["joined"])
        outputs = []
        for extra in ([], ["--root", str(root)]):
            completed = subprocess.run([str(OPTIONS.plain), "verified"] + extra, text=True,
                                       capture_output=True, timeout=30,
                                       creationflags=subprocess.CREATE_NO_WINDOW if run.os.name == "nt" else 0)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            value = json.loads(completed.stdout)
            self.assertEqual(value["status"], 200)
            self.assertEqual(value["additional_root"], bool(extra))
            outputs.append(value)
        (OPTIONS.output / "system-trust.json").write_text(json.dumps(outputs, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--plain", required=True, type=Path)
    parser.add_argument("--meter", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    OPTIONS = parser.parse_args()
    OPTIONS.plain = OPTIONS.plain.resolve()
    OPTIONS.meter = OPTIONS.meter.resolve()
    OPTIONS.output.mkdir(parents=True, exist_ok=False)
    unittest.main(argv=["test_runner.py"], verbosity=2)
