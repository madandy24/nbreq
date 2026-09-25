"""Bounded client-only TLS memory observation; Python 3.8+ and OpenSSL CLI.

The OpenSSL loopback server lives in this controller process. Only the separate
NBReq probe child PID is sampled. No memory threshold is imposed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import socket
import ssl
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


def run_openssl(executable, arguments, directory, log):
    command = [executable] + arguments
    with log.open("a", encoding="utf-8") as output:
        output.write("$ " + " ".join(command) + "\n")
        output.flush()
        result = subprocess.run(command, cwd=str(directory), stdout=output,
                                stderr=subprocess.STDOUT, timeout=15, check=False)
        output.write("exit=" + str(result.returncode) + "\n")
    if result.returncode:
        raise RuntimeError("OpenSSL command failed; see " + str(log))


def certificates(executable, out):
    certs = out / "certs"
    certs.mkdir(mode=0o700)
    (certs / "ca.cnf").write_text(
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3_ca\n"
        "[dn]\nCN=NBReq memory fixture CA\n"
        "[v3_ca]\nbasicConstraints=critical,CA:TRUE\n"
        "keyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n",
        encoding="ascii",
    )
    (certs / "leaf.ext").write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectAltName=IP:127.0.0.1\n",
        encoding="ascii",
    )
    log = out / "openssl.log"
    commands = [
        ["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", "ca.key"],
        ["req", "-x509", "-new", "-key", "ca.key", "-days", "1", "-sha256",
         "-config", "ca.cnf", "-out", "ca.pem"],
        ["genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", "leaf.key"],
        ["req", "-new", "-key", "leaf.key", "-subj", "/CN=127.0.0.1", "-out", "leaf.csr"],
        ["x509", "-req", "-in", "leaf.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
         "-CAcreateserial", "-days", "1", "-sha256", "-extfile", "leaf.ext", "-out", "leaf.pem"],
        ["x509", "-in", "ca.pem", "-outform", "DER", "-out", "ca.der"],
    ]
    for arguments in commands:
        run_openssl(executable, arguments, certs, log)
    for name in ("ca.key", "leaf.key"):
        try:
            (certs / name).chmod(0o600)
        except OSError:
            pass
    return certs


class LocalTlsServer:
    def __init__(self, certs, expected, tls_version):
        self.expected = expected
        self.tls_version = tls_version
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        version = ssl.TLSVersion.TLSv1_2 if tls_version == "1.2" else ssl.TLSVersion.TLSv1_3
        self.context.minimum_version = version
        self.context.maximum_version = version
        self.context.load_cert_chain(str(certs / "leaf.pem"), str(certs / "leaf.key"))
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(expected)
        self.listener.settimeout(0.1)
        self.address = "127.0.0.1:" + str(self.listener.getsockname()[1])
        self.stop_event = threading.Event()
        self.handshakes = 0
        self.negotiated_versions = []
        self.error = None
        self.thread = threading.Thread(target=self._serve, name="bounded-tls-fixture")

    def start(self):
        self.thread.start()

    def _serve(self):
        sessions = []
        deadline = time.monotonic() + RUN_DEADLINE
        try:
            while not self.stop_event.is_set() and time.monotonic() < deadline:
                if self.handshakes == self.expected:
                    self.stop_event.wait(0.05)
                    continue
                try:
                    raw, _ = self.listener.accept()
                except socket.timeout:
                    continue
                except OSError:
                    if self.stop_event.is_set():
                        break
                    raise
                try:
                    raw.settimeout(min(3.0, max(0.1, deadline - time.monotonic())))
                    secured = self.context.wrap_socket(raw, server_side=True)
                except BaseException:
                    raw.close()
                    raise
                sessions.append(secured)
                self.negotiated_versions.append(secured.version())
                self.handshakes += 1
            if not self.stop_event.is_set() and self.handshakes != self.expected:
                raise TimeoutError("TLS fixture did not receive every handshake")
        except BaseException as error:
            self.error = repr(error)
        finally:
            self.listener.close()
            for secured in sessions:
                try:
                    secured.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                secured.close()

    def stop(self):
        self.stop_event.set()
        self.listener.close()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError("TLS fixture thread did not join within five seconds")
        if self.error:
            raise RuntimeError("TLS fixture: " + self.error)
        if self.handshakes != self.expected:
            raise RuntimeError("TLS fixture handshake count {} != {}".format(
                self.handshakes, self.expected))
        expected_version = "TLSv" + self.tls_version
        if self.negotiated_versions != [expected_version] * self.expected:
            raise RuntimeError("unexpected negotiated TLS versions: " +
                               repr(self.negotiated_versions))


def stop_owned_process(process, log):
    if process.poll() is not None:
        return
    if os.name == "nt":
        with log.open("a", encoding="utf-8") as output:
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                           stdout=output, stderr=subprocess.STDOUT, timeout=15, check=False)
    else:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    process.wait(timeout=15)


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


def observe(binary, certs, count, out, tls_version):
    run_dir = out / ("count-" + str(count))
    run_dir.mkdir()
    fixture = LocalTlsServer(certs, count, tls_version)
    fixture.start()
    raw_log = run_dir / "probe.stdout.log"
    phases = []
    samples = []
    lines = queue.Queue()
    argv = [str(binary), "hold", fixture.address, "127.0.0.1", str(certs / "ca.der"), str(count)]
    process = None
    sampler = None
    reader = None
    try:
        process = subprocess.Popen(argv, cwd=str(ROOT), stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   text=True, bufsize=1,
                                   creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0,
                                   start_new_session=os.name != "nt")
        sampler = Sampler(process.pid)
        start = time.monotonic()

        def read_output():
            with raw_log.open("w", encoding="utf-8") as output:
                for line in process.stdout:
                    output.write(line)
                    output.flush()
                    lines.put((time.monotonic() - start, line.rstrip("\r\n")))

        reader = threading.Thread(target=read_output, name="probe-output")
        reader.start()
        phase = "launch"
        while True:
            while not lines.empty():
                observed_at, line = lines.get_nowait()
                if not line.startswith("phase="):
                    continue
                fields = dict(part.split("=", 1) for part in line.split())
                phase = fields["phase"]
                phases.append({"time_s": observed_at, "phase": phase,
                               "count": int(fields["count"]),
                               "reserved": int(fields["reserved"])})
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
        if process.returncode:
            raise RuntimeError("probe exited {}; see {}".format(process.returncode, raw_log))
        expected_reserve = count * (2 * WINDOW + TLS_RESERVE)
        expected = [("baseline", 0, 0), ("connecting", 0, 0),
                    ("ready", count, expected_reserve),
                    ("closing", count, expected_reserve), ("released", 0, 0)]
        actual = [(item["phase"], item["count"], item["reserved"]) for item in phases]
        if actual != expected:
            raise RuntimeError("probe phase/count/reserve mismatch: " + repr(actual))
        if fixture.handshakes != count:
            raise RuntimeError("fixture handshake count mismatch")
        if fixture.negotiated_versions != ["TLSv" + tls_version] * count:
            raise RuntimeError("fixture negotiated an unexpected protocol version")
        report = {"count": count, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                  "address": fixture.address, "handshakes": fixture.handshakes,
                  "tls_version": tls_version,
                  "negotiated_versions": fixture.negotiated_versions,
                  "exit_code": process.returncode, "phases": phases,
                  "summary": summarize(samples)}
        (run_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return report
    finally:
        primary_failure = sys.exc_info()[0] is not None
        cleanup_errors = []
        try:
            if process is not None:
                stop_owned_process(process, raw_log)
        except BaseException as error:
            cleanup_errors.append("probe stop: " + repr(error))
            if process is not None and process.poll() is None:
                try:
                    process.kill()  # Only this Popen-owned child is a fallback target.
                    process.wait(timeout=5)
                except BaseException as fallback:
                    cleanup_errors.append("probe kill fallback: " + repr(fallback))
        try:
            if sampler is not None:
                sampler.close()
        except BaseException as error:
            cleanup_errors.append("sampler close: " + repr(error))
        try:
            if reader is not None:
                reader.join(timeout=5)
                if reader.is_alive():
                    raise RuntimeError("probe log reader did not join")
        except BaseException as error:
            cleanup_errors.append("reader join: " + repr(error))
        try:
            fixture.stop()
        except BaseException as error:
            cleanup_errors.append("fixture stop: " + repr(error))
        try:
            (run_dir / "samples.jsonl").write_text(
                "".join(json.dumps(row, sort_keys=True) + "\n" for row in samples),
                encoding="utf-8")
            (run_dir / "phases.json").write_text(json.dumps(phases, indent=2) + "\n",
                                                 encoding="utf-8")
        except BaseException as error:
            cleanup_errors.append("sample persistence: " + repr(error))
        if cleanup_errors:
            try:
                (run_dir / "cleanup-errors.log").write_text(
                    "\n".join(cleanup_errors) + "\n", encoding="utf-8")
            except OSError:
                pass
            if not primary_failure:
                raise RuntimeError("memory observation cleanup failed: " + "; ".join(cleanup_errors))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path, help="new output directory outside source")
    parser.add_argument("--binary", required=True, type=Path, help="already-built probe executable")
    parser.add_argument("--openssl", default=shutil.which("openssl") or "openssl")
    parser.add_argument("--tls-version", choices=("1.2", "1.3"), default="1.3")
    args = parser.parse_args()
    if args.tls_version == "1.3" and not ssl.HAS_TLSv1_3:
        raise SystemExit("this Python SSL runtime cannot host TLS 1.3; use --tls-version 1.2")
    out = args.out.resolve()
    binary = args.binary.resolve()
    if out.exists():
        raise SystemExit("output directory must be new: " + str(out))
    if not binary.is_file():
        raise SystemExit("probe binary is absent: " + str(binary))
    if Path(__file__).resolve().parent in out.parents:
        raise SystemExit("output directory must be outside probe source")
    out.mkdir(mode=0o700, parents=True)
    certs = out / "certs"
    try:
        certificates(args.openssl, out)
        reports = [observe(binary, certs, count, out, args.tls_version)
                   for count in COUNT_CHOICES]
        (out / "report.json").write_text(json.dumps(reports, indent=2) + "\n", encoding="utf-8")
        print("client-only TLS memory observations saved to " + str(out))
    except BaseException as error:
        (out / "failure.txt").write_text(repr(error) + "\n", encoding="utf-8")
        raise
    finally:
        if certs.exists():
            for name in ("ca.key", "leaf.key"):
                path = certs / name
                if path.exists():
                    path.unlink()


if __name__ == "__main__":
    main()
