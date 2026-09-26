"""Process-level fault tests for the bounded TLS memory controller.

The fake children exercise ownership and evidence handling without pretending to
provide TLS. The Rust fixture's TLS behavior is tested separately with real
verified handshakes in tools/tcp-tls-fixture/tests/fixture_process.rs.
"""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("tls_memory_controller_under_test", HERE / "memory.py")
memory = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(memory)


FAKE_FIXTURE = r'''
import argparse
import json
import os
from pathlib import Path
import socket
import sys
import time

p = argparse.ArgumentParser()
p.add_argument("--expected", type=int, required=True)
p.add_argument("--tls-version", required=True)
p.add_argument("--ca-der", type=Path, required=True)
p.add_argument("--ready", type=Path, required=True)
p.add_argument("--report", type=Path, required=True)
a = p.parse_args()
mode = os.environ.get("FAKE_FIXTURE_MODE", "normal")
if mode == "early_exit":
    sys.exit(7)
listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.bind(("127.0.0.1", 0))
listener.listen(1)
a.ca_der.write_bytes(b"public root placeholder")
ready = {"pid": os.getpid(), "address": "127.0.0.1:" + str(listener.getsockname()[1]),
         "expected": a.expected, "tls_version": a.tls_version}
if mode == "wrong_pid":
    ready["pid"] = int(os.environ.get("FAKE_READY_PID", os.getpid() + 900000))
elif mode == "wrong_address":
    ready["address"] = "192.0.2.1:23451"
elif mode == "wrong_version":
    ready["tls_version"] = "1.2" if a.tls_version == "1.3" else "1.3"
elif mode == "wrong_count":
    ready["expected"] = a.expected + 1
if mode != "never_ready":
    a.ready.write_text(json.dumps(ready), encoding="utf-8")
if mode == "dies_after_ready":
    time.sleep(0.25)
    sys.exit(8)
if mode == "never_ready":
    time.sleep(30)
    sys.exit(9)
if sys.stdin.readline() != "STOP\n":
    sys.exit(10)
report = {"status": "passed", "pid": os.getpid(), "address": ready["address"],
          "expected": a.expected, "accepted": a.expected,
          "handshakes": a.expected, "tls_version": a.tls_version,
          "negotiated_versions": ["TLSv" + a.tls_version] * a.expected}
if mode == "wrong_report_pid":
    report["pid"] += 900000
elif mode == "wrong_report_count":
    report["handshakes"] -= 1
elif mode == "wrong_report_version":
    report["negotiated_versions"][-1] = "TLSv1.2" if a.tls_version == "1.3" else "TLSv1.3"
elif mode == "failed_report":
    report["status"] = "failed"
a.report.write_text(json.dumps(report), encoding="utf-8")
sys.exit(1 if report["status"] == "failed" else 0)
'''


FAKE_PROBE = r'''
import os
import sys
import time

count = int(sys.argv[-1])
reserved = count * (2 * 16 * 1024 + 256 * 1024)
print("phase=baseline count=0 reserved=0", flush=True)
if os.environ.get("FAKE_PROBE_MODE") == "hang":
    time.sleep(30)
    sys.exit(9)
time.sleep(0.16)
print("phase=connecting count=0 reserved=0", flush=True)
print(f"phase=ready count={count} reserved={reserved}", flush=True)
time.sleep(0.16)
print(f"phase=closing count={count} reserved={reserved}", flush=True)
print("phase=released count=0 reserved=0", flush=True)
time.sleep(0.16)
'''


class ControllerFaultTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="nbreq-memory-test-")
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.fixture_marker = self.folder / "fake-fixture"
        self.probe_marker = self.folder / "fake-probe"
        self.fixture_marker.write_bytes(b"fixture marker")
        self.probe_marker.write_bytes(b"probe marker")
        self.fixture_script = self.folder / "fixture.py"
        self.probe_script = self.folder / "probe.py"
        self.fixture_script.write_text(FAKE_FIXTURE, encoding="utf-8")
        self.probe_script.write_text(FAKE_PROBE, encoding="utf-8")
        self.children = []
        self.attempt = 0
        self.out = self.folder / "evidence-0"

    def next_attempt(self):
        self.attempt += 1
        self.out = self.folder / "evidence-{}".format(self.attempt)
        self.children = []

    def run_controller(self, fixture_mode="normal", probe_mode="normal", *,
                       fail_probe_launch=False, fail_sampler=False, deadline=2.0,
                       path_write_failure=None, log_open_failure=False,
                       expect_success=False, unrelated_pid=None,
                       expected_error=None, report_publish_failure=False):
        original_popen = subprocess.Popen
        original_write_text = Path.write_text
        original_open = Path.open
        original_replace = os.replace

        def launch(command, *args, **kwargs):
            executable = os.fspath(command[0])
            if executable == str(self.fixture_marker):
                command = [sys.executable, str(self.fixture_script)] + list(command[1:])
            elif executable == str(self.probe_marker):
                if fail_probe_launch:
                    raise OSError("injected probe launch failure")
                command = [sys.executable, str(self.probe_script)] + list(command[1:])
            child = original_popen(command, *args, **kwargs)
            if executable in (str(self.fixture_marker), str(self.probe_marker)):
                self.children.append(child)
            return child

        def write_text(path, text, *args, **kwargs):
            if path_write_failure and path.name == path_write_failure:
                raise OSError("injected evidence write failure")
            return original_write_text(path, text, *args, **kwargs)

        def open_file(path, *args, **kwargs):
            if log_open_failure and path.name == "probe.stdout.log":
                handle = original_open(path, *args, **kwargs)

                class FailAfterRead:
                    def __enter__(self):
                        return handle.__enter__()

                    def __exit__(self, exc_type, exc_value, traceback):
                        handle.__exit__(exc_type, exc_value, traceback)
                        raise OSError("injected raw log reader failure after complete output")

                return FailAfterRead()
            return original_open(path, *args, **kwargs)

        def replace(source, destination):
            if report_publish_failure and Path(destination) == self.out / "count-16" / "report.json":
                raise OSError("injected report publish failure")
            return original_replace(source, destination)

        class BrokenSampler:
            def __init__(self, _pid):
                raise OSError("injected sampler constructor failure")

        argv = ["memory.py", "--binary", str(self.probe_marker),
                "--fixture-binary", str(self.fixture_marker),
                "--out", str(self.out), "--tls-version", "1.3"]
        with mock.patch.object(sys, "argv", argv), \
             mock.patch.object(memory, "COUNT_CHOICES", (16,)), \
             mock.patch.object(memory, "RUN_DEADLINE", deadline), \
             mock.patch.object(memory.subprocess, "Popen", side_effect=launch), \
             mock.patch.object(Path, "write_text", write_text), \
             mock.patch.object(Path, "open", open_file), \
             mock.patch.object(memory.os, "replace", side_effect=replace), \
             mock.patch.dict(os.environ, {"FAKE_FIXTURE_MODE": fixture_mode,
                                       "FAKE_PROBE_MODE": probe_mode,
                                       "FAKE_READY_PID": str(unrelated_pid or 0)}), \
             mock.patch.object(memory, "Sampler", BrokenSampler if fail_sampler else memory.Sampler):
            if expect_success:
                memory.main()
            else:
                with self.assertRaises(Exception) as caught:
                    memory.main()
                if expected_error is not None:
                    self.assertIn(expected_error, str(caught.exception))
        self.assert_reaped()
        if expect_success:
            self.assertTrue((self.out / "report.json").exists(),
                            "positive control did not publish its final report")
            self.assertTrue((self.out / "count-16" / "report.json").exists())
        else:
            for path in (self.out / "report.json", self.out / "count-16" / "report.json"):
                self.assertFalse(path.exists(),
                                 "a failed observation published a success report: " + str(path))

    def assert_reaped(self):
        self.assertTrue(self.children, "controller did not start the fake fixture")
        for child in self.children:
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=3)
                self.fail("controller left owned child PID {} running".format(child.pid))

    def test_ready_metadata_must_match_owned_process_and_requested_fixture(self):
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(unrelated.wait)
        self.addCleanup(lambda: unrelated.kill() if unrelated.poll() is None else None)
        reasons = {"wrong_pid": "readiness PID", "wrong_address": "invalid loopback",
                   "wrong_version": "readiness TLS version",
                   "wrong_count": "readiness count"}
        for mode, reason in reasons.items():
            with self.subTest(mode=mode):
                self.run_controller(fixture_mode=mode, unrelated_pid=unrelated.pid,
                                    expected_error=reason)
                self.assertEqual(len(self.children), 1,
                                 "invalid readiness must not launch the probe")
                self.assertIsNone(unrelated.poll(),
                                  "controller killed a process it did not launch")
                self.next_attempt()

    def test_fixture_exit_before_and_after_readiness_reaps_owned_children(self):
        self.run_controller(fixture_mode="early_exit", expected_error="before readiness")
        self.next_attempt()
        self.run_controller(fixture_mode="dies_after_ready", probe_mode="hang",
                            expected_error="before observer requested stop")

    def test_partial_probe_launch_and_sampler_failure_reap_fixture(self):
        self.run_controller(fail_probe_launch=True, expected_error="injected probe launch")
        self.assertEqual(len(self.children), 1)
        self.next_attempt()
        self.run_controller(fail_sampler=True, expected_error="injected sampler constructor")
        self.assertEqual(len(self.children), 2)

    def test_observer_deadline_reaps_both_children(self):
        self.run_controller(probe_mode="hang", deadline=0.65, expected_error="probe exceeded")
        self.assertEqual(len(self.children), 2)

    def test_invalid_terminal_reports_cannot_publish_success(self):
        reasons = {"wrong_report_pid": "report pid mismatch",
                   "wrong_report_count": "handshake count mismatch",
                   "wrong_report_version": "negotiated unexpected versions",
                   "failed_report": "fixture exited 1"}
        for mode, reason in reasons.items():
            with self.subTest(mode=mode):
                self.run_controller(fixture_mode=mode, expected_error=reason)
                self.assertEqual(len(self.children), 2)
                self.next_attempt()

    def test_evidence_write_failure_does_not_publish_success(self):
        self.run_controller(path_write_failure="samples.jsonl",
                            expected_error="sample persistence")
        self.assertEqual(len(self.children), 2)

    def test_report_publish_failure_leaves_no_visible_success_file(self):
        self.run_controller(report_publish_failure=True,
                            expected_error="injected report publish failure")
        self.assertEqual(len(self.children), 2)

    def test_raw_log_reader_failure_reaps_children_and_blocks_success(self):
        self.run_controller(log_open_failure=True, expected_error="reader")
        self.assertEqual(len(self.children), 2)

    def test_valid_fake_children_are_a_positive_control_for_controller_validation(self):
        self.run_controller(expect_success=True)
        self.assertEqual(len(self.children), 2)


if __name__ == "__main__":
    unittest.main()
