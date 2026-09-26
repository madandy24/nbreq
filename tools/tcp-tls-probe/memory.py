"""Bounded TLS memory observation of only the NBReq client process (Python 3.8+)."""

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import queue
import signal
import statistics
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "release-soak"))
from process_sample import Sampler  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
COUNT_CHOICES = (16, 32)
WINDOW = 16 * 1024
TLS_RESERVE = 256 * 1024
RUN_DEADLINE = 120
SAMPLE_INTERVAL = 0.05


def publish_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def metadata(path, limit):
    if path.stat().st_size > limit:
        raise RuntimeError("fixture metadata exceeds byte cap: " + str(path))
    return json.loads(path.read_text(encoding="utf-8"))


def validate_ready(ready, fixture, count, version):
    if not isinstance(ready, dict):
        raise RuntimeError("fixture readiness is not an object")
    if type(ready.get("pid")) is not int or ready["pid"] != fixture.pid:
        raise RuntimeError("fixture readiness PID does not match owned child")
    if type(ready.get("expected")) is not int or ready["expected"] != count:
        raise RuntimeError("fixture readiness count mismatch")
    if ready.get("tls_version") != version:
        raise RuntimeError("fixture readiness TLS version mismatch")
    address = ready.get("address")
    try:
        host, port = address.rsplit(":", 1)
        if ipaddress.ip_address(host) != ipaddress.IPv4Address("127.0.0.1"):
            raise ValueError("not IPv4 loopback")
        if not 0 < int(port) < 65536:
            raise ValueError("invalid port")
    except (AttributeError, ValueError) as error:
        raise RuntimeError("fixture advertised an invalid loopback address") from error
    return ready


def validate_report(report, ready, fixture, count, version):
    if not isinstance(report, dict) or report.get("status") != "passed":
        raise RuntimeError("fixture did not report success")
    for key in ("pid", "address", "expected", "tls_version"):
        if report.get(key) != ready[key]:
            raise RuntimeError("fixture report {} mismatch".format(key))
    if report["pid"] != fixture.pid or report["expected"] != count:
        raise RuntimeError("fixture report does not match owned child")
    if type(report.get("accepted")) is not int or report["accepted"] != count:
        raise RuntimeError("fixture accepted count mismatch")
    if type(report.get("handshakes")) is not int or report["handshakes"] != count:
        raise RuntimeError("fixture handshake count mismatch")
    if report["tls_version"] != version:
        raise RuntimeError("fixture protocol mismatch")
    if report.get("negotiated_versions") != ["TLSv" + version] * count:
        raise RuntimeError("fixture negotiated unexpected versions")
    return report


def stop_owned_process(child):
    """Terminate and reap only a Popen-owned child PID."""
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)
    else:
        child.wait(timeout=5)


def summarize(samples):
    result = {}
    for phase in ("baseline", "ready", "released"):
        rows = [row for row in samples if row["phase"] == phase]
        if not rows:
            raise RuntimeError("no client samples for phase " + phase)
        metrics = {}
        for key in ("rss", "private", "lifetime_peak_rss", "cpu_seconds"):
            values = [row[key] for row in rows if row.get(key) is not None]
            if values:
                metrics[key] = {"min": min(values), "median": statistics.median(values),
                                "max": max(values)}
                if key == "cpu_seconds":
                    metrics[key]["delta"] = max(values) - min(values)
        result[phase] = {"samples": len(rows), "metrics": metrics}
    return result


def wait_ready(path, fixture, count, version):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if path.exists():
            return validate_ready(metadata(path, 16 * 1024), fixture, count, version)
        if fixture.poll() is not None:
            raise RuntimeError("fixture exited before readiness")
        time.sleep(0.01)
    raise TimeoutError("fixture readiness exceeded eight seconds")


def observe(binary, fixture_binary, count, out, tls_version):
    run_dir = out / ("count-" + str(count))
    run_dir.mkdir()
    ca_der = run_dir / "root.der"
    ready_path = run_dir / "ready.json"
    fixture_report = run_dir / "fixture-report.json"
    fixture_log = run_dir / "fixture.log"
    raw_log = run_dir / "probe.stdout.log"
    phases, samples, reader_errors = [], [], []
    lines = queue.Queue()
    fixture = process = sampler = reader = log_handle = None
    result = None
    cleanup_errors = []
    try:
        log_handle = fixture_log.open("wb")
        fixture = subprocess.Popen(
            [str(fixture_binary), "--expected", str(count), "--tls-version", tls_version,
             "--ca-der", str(ca_der), "--ready", str(ready_path), "--report", str(fixture_report)],
            cwd=str(ROOT), stdin=subprocess.PIPE, stdout=log_handle, stderr=subprocess.STDOUT)
        ready = wait_ready(ready_path, fixture, count, tls_version)
        if not ca_der.is_file() or ca_der.stat().st_size > 64 * 1024:
            raise RuntimeError("fixture public CA is absent or oversized")
        process = subprocess.Popen(
            [str(binary), "hold", ready["address"], "127.0.0.1", str(ca_der), str(count)],
            cwd=str(ROOT), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, bufsize=1)
        start = time.monotonic()

        def read_output():
            try:
                with raw_log.open("w", encoding="utf-8") as output:
                    for line in process.stdout:
                        output.write(line)
                        output.flush()
                        lines.put((time.monotonic() - start, line.rstrip("\r\n")))
            except BaseException as error:
                reader_errors.append(error)

        reader = threading.Thread(target=read_output, name="probe-output")
        reader.start()
        sampler = Sampler(process.pid)
        phase = "launch"
        while True:
            while not lines.empty():
                observed_at, line = lines.get_nowait()
                if not line.startswith("phase="):
                    continue
                fields = dict(part.split("=", 1) for part in line.split())
                phase = fields["phase"]
                phases.append({"time_s": observed_at, "phase": phase,
                               "count": int(fields["count"]), "reserved": int(fields["reserved"])})
            if reader_errors:
                raise RuntimeError("probe raw log reader failed: " + repr(reader_errors[0]))
            if fixture.poll() is not None:
                raise RuntimeError("fixture exited before observer requested stop")
            if process.poll() is not None and not reader.is_alive() and lines.empty():
                break
            if time.monotonic() - start >= RUN_DEADLINE:
                raise TimeoutError("probe exceeded 120 seconds")
            if process.poll() is None:
                try:
                    sample = sampler.sample()
                    sample.update(time_s=time.monotonic() - start, phase=phase)
                    samples.append(sample)
                except (FileNotFoundError, ProcessLookupError, KeyError, IndexError,
                        subprocess.CalledProcessError):
                    if process.poll() is None:
                        raise
            time.sleep(SAMPLE_INTERVAL)
        process.wait(timeout=5)
        reader.join(timeout=5)
        if reader.is_alive():
            raise RuntimeError("probe log reader did not join")
        if reader_errors:
            raise RuntimeError("probe raw log reader failed: " + repr(reader_errors[0]))
        if process.returncode:
            raise RuntimeError("probe exited {}; see {}".format(process.returncode, raw_log))
        expected_reserve = count * (2 * WINDOW + TLS_RESERVE)
        expected = [("baseline", 0, 0), ("connecting", 0, 0),
                    ("ready", count, expected_reserve),
                    ("closing", count, expected_reserve), ("released", 0, 0)]
        actual = [(item["phase"], item["count"], item["reserved"]) for item in phases]
        if actual != expected:
            raise RuntimeError("probe phase/count/reserve mismatch: " + repr(actual))
        fixture.stdin.write(b"STOP\n")
        fixture.stdin.flush()
        fixture.wait(timeout=5)
        if fixture.returncode:
            raise RuntimeError("fixture exited {}; see {}".format(fixture.returncode, fixture_log))
        if not fixture_report.exists():
            raise RuntimeError("fixture did not write terminal report")
        report = validate_report(metadata(fixture_report, 64 * 1024),
                                 ready, fixture, count, tls_version)
        result = {"count": count, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                  "fixture_binary_sha256": hashlib.sha256(fixture_binary.read_bytes()).hexdigest(),
                  "address": ready["address"], "handshakes": report["handshakes"],
                  "tls_version": tls_version, "negotiated_versions": report["negotiated_versions"],
                  "exit_code": process.returncode, "phases": phases, "summary": summarize(samples)}
    finally:
        original_failure = sys.exc_info()[0] is not None
        for label, child in (("probe", process), ("fixture", fixture)):
            if child is not None:
                try:
                    stop_owned_process(child)
                except BaseException as error:
                    cleanup_errors.append(label + " stop: " + repr(error))
        if sampler is not None:
            try:
                sampler.close()
            except BaseException as error:
                cleanup_errors.append("sampler close: " + repr(error))
        if reader is not None:
            try:
                reader.join(timeout=5)
                if reader.is_alive():
                    raise RuntimeError("probe log reader did not join")
            except BaseException as error:
                cleanup_errors.append("reader join: " + repr(error))
        for label, stream in (("probe stdout", process.stdout if process is not None else None),
                              ("fixture stdin", fixture.stdin if fixture is not None else None)):
            if stream is not None:
                try:
                    stream.close()
                except BaseException as error:
                    cleanup_errors.append(label + " close: " + repr(error))
        if log_handle is not None:
            try:
                log_handle.close()
            except BaseException as error:
                cleanup_errors.append("fixture log close: " + repr(error))
        try:
            (run_dir / "samples.jsonl").write_text(
                "".join(json.dumps(row, sort_keys=True) + "\n" for row in samples), encoding="utf-8")
            (run_dir / "phases.json").write_text(json.dumps(phases, indent=2) + "\n", encoding="utf-8")
        except BaseException as error:
            cleanup_errors.append("sample persistence: " + repr(error))
        if cleanup_errors:
            try:
                (run_dir / "cleanup-errors.log").write_text(
                    "\n".join(cleanup_errors) + "\n", encoding="utf-8")
            except OSError:
                pass
            if not original_failure:
                raise RuntimeError("memory observation cleanup failed: " + "; ".join(cleanup_errors))
    publish_json(run_dir / "report.json", result)
    return result


def terminate_on_sigterm(_signal, _frame):
    raise SystemExit("memory observer received SIGTERM")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path, help="new output directory outside source")
    parser.add_argument("--binary", required=True, type=Path, help="already-built probe executable")
    parser.add_argument("--fixture-binary", required=True, type=Path, help="already-built Rust fixture")
    parser.add_argument("--tls-version", choices=("1.2", "1.3"), default="1.3")
    args = parser.parse_args()
    out = args.out.resolve()
    binary = args.binary.resolve()
    fixture_binary = args.fixture_binary.resolve()
    if out.exists():
        raise SystemExit("output directory must be new: " + str(out))
    if not binary.is_file() or not fixture_binary.is_file():
        raise SystemExit("probe and fixture binaries must already exist")
    if Path(__file__).resolve().parent in out.parents:
        raise SystemExit("output directory must be outside probe source")
    previous_sigterm = signal.signal(signal.SIGTERM, terminate_on_sigterm)
    try:
        out.mkdir(mode=0o700, parents=True)
        reports = [observe(binary, fixture_binary, count, out, args.tls_version)
                   for count in COUNT_CHOICES]
        publish_json(out / "report.json", reports)
        print("client-only TLS memory observations saved to " + str(out))
    except BaseException as error:
        if out.exists():
            try:
                (out / "failure.txt").write_text(repr(error) + "\n", encoding="utf-8")
            except OSError:
                pass
        raise
    finally:
        signal.signal(signal.SIGTERM, previous_sigterm)


if __name__ == "__main__":
    main()
