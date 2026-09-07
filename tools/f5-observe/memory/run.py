"""Bounded F5 controller. Python 3.8+, standard library only; no production dependency.

The fixture is a separate, always uninstrumented process. Phase RAM peaks are sampled,
not exact maxima. Rust requested-heap counters come from the optional client meter.
Only PIDs created by this invocation can be stopped. Failed cases never enter results.
"""

import argparse
import ctypes
import datetime
import hashlib
import itertools
import json
import os
from pathlib import Path
import platform
import queue
import subprocess
import sys
import threading
import time


class Sampler:
    def __init__(self, pid):
        self.pid = pid
        self.handle = None
        if os.name == "nt":
            from ctypes import wintypes as wt

            class Memory(ctypes.Structure):
                _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD)] + [
                    (name, ctypes.c_size_t) for name in (
                        "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                        "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
                        "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]

            self.memory_type = Memory
            self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
            self.kernel.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
            self.kernel.OpenProcess.restype = wt.HANDLE
            self.kernel.CloseHandle.argtypes = [wt.HANDLE]
            self.kernel.GetProcessTimes.argtypes = [wt.HANDLE] + [ctypes.POINTER(wt.FILETIME)] * 4
            self.psapi.GetProcessMemoryInfo.argtypes = [wt.HANDLE, ctypes.POINTER(Memory), wt.DWORD]
            self.filetime = wt.FILETIME
            self.handle = self.kernel.OpenProcess(0x0400 | 0x0010, False, pid)
            if not self.handle:
                raise ctypes.WinError(ctypes.get_last_error())
        elif sys.platform.startswith("linux"):
            self.ticks = os.sysconf("SC_CLK_TCK")
        elif sys.platform != "darwin":
            raise RuntimeError("unsupported process sampler: " + sys.platform)

    def sample(self):
        if self.handle:
            memory = self.memory_type()
            memory.cb = ctypes.sizeof(memory)
            if not self.psapi.GetProcessMemoryInfo(self.handle, ctypes.byref(memory), memory.cb):
                raise ctypes.WinError(ctypes.get_last_error())
            times = [self.filetime() for _ in range(4)]
            if not self.kernel.GetProcessTimes(self.handle, *[ctypes.byref(item) for item in times]):
                raise ctypes.WinError(ctypes.get_last_error())
            cpu = sum((item.dwHighDateTime << 32) + item.dwLowDateTime for item in times[2:]) / 1e7
            return dict(rss=memory.WorkingSetSize, private=memory.PrivateUsage,
                        lifetime_peak_rss=memory.PeakWorkingSetSize, cpu_seconds=cpu)
        if sys.platform.startswith("linux"):
            folder = Path("/proc") / str(self.pid)
            status = {}
            for line in (folder / "status").read_text().splitlines():
                name, _, value = line.partition(":")
                if name in ("VmRSS", "VmHWM", "VmSize"):
                    status[name] = int(value.split()[0]) * 1024
            fields = (folder / "stat").read_text().rsplit(")", 1)[1].split()
            private = None
            try:
                private = sum(int(line.split()[1]) * 1024 for line in
                              (folder / "smaps_rollup").read_text().splitlines()
                              if line.startswith(("Private_Clean:", "Private_Dirty:")))
            except PermissionError:
                pass
            return dict(rss=status["VmRSS"], private=private, virtual=status["VmSize"],
                        lifetime_peak_rss=status["VmHWM"],
                        cpu_seconds=(int(fields[11]) + int(fields[12])) / self.ticks)
        # ps is intentionally sampled less often on Macs; no private/footprint claim.
        line = subprocess.check_output(
            ["ps", "-o", "rss=", "-o", "vsz=", "-o", "time=", "-p", str(self.pid)],
            text=True, timeout=2).split()
        parts = line[2].split(":")
        cpu = sum(float(value) * 60 ** index for index, value in enumerate(reversed(parts)))
        return dict(rss=int(line[0]) * 1024, private=None, virtual=int(line[1]) * 1024,
                    lifetime_peak_rss=None, cpu_seconds=cpu)

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)
            self.handle = None


class Observation:
    def __init__(self):
        self.first = None
        self.last = None
        self.peaks = {}
        self.count = 0

    def add(self, value):
        if self.first is None:
            self.first = value
        self.last = value
        self.count += 1
        for name, amount in value.items():
            if name != "cpu_seconds" and amount is not None:
                self.peaks[name] = max(amount, self.peaks.get(name, 0))

    def result(self):
        return dict(start=self.first, end=self.last, sampled_peaks=self.peaks, samples=self.count,
                    cpu_seconds=self.last["cpu_seconds"] - self.first["cpu_seconds"])


class Child:
    def __init__(self, command, log):
        self.stderr = open(log, "w", encoding="utf-8")
        self.process = subprocess.Popen(
            [str(item) for item in command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr, text=True, bufsize=1,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        self.events = queue.Queue()
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.sampler = None

    def _read(self):
        try:
            for line in self.process.stdout:
                try:
                    self.events.put(json.loads(line))
                except ValueError as error:
                    self.events.put(error)
        finally:
            self.events.put(EOFError("child stdout ended"))

    def receive(self, timeout):
        event = self.events.get(timeout=max(0.001, timeout))
        if isinstance(event, Exception):
            raise event
        return event

    def ack(self):
        # Send the protocol byte unchanged; Windows TextIOWrapper translates LF to CRLF.
        self.process.stdin.buffer.write(b"\n")
        self.process.stdin.buffer.flush()

    def observe(self):
        if self.sampler is None:
            self.sampler = Sampler(self.process.pid)
        return self.sampler.sample()

    def close(self):
        forced = False
        if self.process.poll() is None:
            try:
                self.ack()
                self.process.wait(timeout=3)
            except (subprocess.TimeoutExpired, BrokenPipeError, OSError):
                forced = True
                self.process.kill()
                self.process.wait(timeout=5)
        self.reader.join(timeout=3)
        if self.reader.is_alive():
            raise RuntimeError("child output reader failed to join")
        if self.sampler:
            self.sampler.close()
        try:
            self.process.stdin.close()
        except (BrokenPipeError, OSError):
            pass  # The joined child can have exited without consuming a final acknowledgement.
        self.process.stdout.close()
        self.stderr.close()
        return dict(pid=self.process.pid, exit_code=self.process.returncode, forced=forced, joined=True)


def run_case(plain, binary, folder, source, protocol, connections, size, rounds,
             extended=False, timeout=45, corrupt=False):
    folder = Path(folder)
    folder.mkdir(parents=True, exist_ok=False)
    children = []
    records = []
    result = None
    failure = None
    fixture_observation = Observation()
    interval = 0.1 if sys.platform == "darwin" else 0.01
    deadline = time.monotonic() + timeout
    try:
        fixture = Child([plain, "fixture", "--tls", "yes" if protocol == "https" else "no",
                         "--corrupt", "yes" if corrupt else "no"], folder / "fixture.stderr")
        children.append(fixture)
        ready = fixture.receive(min(10, timeout))
        if ready.get("event") != "fixture_ready":
            raise ValueError("fixture did not become ready")
        root = folder / "root.der"
        root.write_bytes(bytes(ready.pop("root_der")))
        command = [binary, "client", "--base", protocol + "://" + ready["address"],
                   "--connections", connections, "--body-bytes", size, "--rounds", rounds,
                   "--extended", "yes" if extended else "no", "--source", source]
        if protocol == "https":
            command += ["--root", root]
        client = Child(command, folder / "client.stderr")
        children.append(client)
        active = None
        observation = None
        next_sample = 0
        client_ready = None
        done = None
        while done is None:
            if time.monotonic() >= deadline:
                raise TimeoutError("case exceeded %.1f seconds" % timeout)
            if active and time.monotonic() >= next_sample:
                observation.add(client.observe())
                fixture_observation.add(fixture.observe())
                next_sample = time.monotonic() + interval
            try:
                event = client.receive(min(interval, deadline - time.monotonic()))
            except queue.Empty:
                continue
            records.append(event)
            kind = event.get("event")
            if kind == "client_ready":
                client_ready = event
                if event["source"] != source or event["connections"] != connections:
                    raise ValueError("client provenance/workload mismatch")
            elif kind == "phase_begin":
                if active is not None:
                    raise ValueError("overlapping phases")
                active = event["phase"]
                observation = Observation()
                observation.add(client.observe())
                fixture_observation.add(fixture.observe())
                next_sample = time.monotonic() + interval
                client.ack()
            elif kind == "phase_end":
                if active != event["phase"]:
                    raise ValueError("unmatched phase end")
                observation.add(client.observe())
                fixture_observation.add(fixture.observe())
                event["process"] = observation.result()
                active = None
                client.ack()
            elif kind == "client_done":
                done = event
            else:
                raise ValueError("unexpected client event: " + str(kind))
        if active or client_ready is None:
            raise ValueError("incomplete client protocol")
        if client.process.wait(timeout=5) != 0:
            raise RuntimeError("client failed after final event")
        for check in ("exact_bytes", "exact_accounting", "quiescent", "joined_shutdown"):
            if done["checks"].get(check) is not True:
                raise ValueError("client check failed: " + check)
        if protocol == "https" and done["checks"].get("verified_tls") is not True:
            raise ValueError("unverified TLS measurement")
        phases = [item for item in records if item["event"] == "phase_end"]
        expected = ["driver_idle", "engine_idle", "cold_connections", "steady", "burst_retained", "released_idle"]
        if extended:
            expected += ["slow_peer", "long_polls_and_burst", "cancel_and_replace", "slow_reader_held",
                         "slow_reader_drain", "large_transfer_retained", "post_large_idle"]
        expected += ["shutdown_idle"]
        if [item["phase"] for item in phases] != expected:
            raise ValueError("missing or unexpected workload phase")
        fixture.ack()
        stopped = fixture.receive(timeout=5)
        if stopped.get("event") != "fixture_stopped" or not stopped.get("joined"):
            raise ValueError("fixture shutdown did not join")
        if fixture.process.wait(timeout=5) != 0:
            raise RuntimeError("fixture failed after shutdown")
        stats = stopped["stats"]
        if stats["bad_body"] or stats["active_requests"]:
            raise ValueError("fixture failed exact upload bytes or release")
        if stats["high_active_requests"] < connections:
            raise ValueError("fixture did not observe requested concurrency")
        metrics = done["metrics"]
        if not extended and (stats["connections"] != connections or stats["aborted"]
                             or stats["requests"] != metrics["accepted"]
                             or stats["completed"] != metrics["completed"]):
            raise ValueError("fixture and client accounting disagree")
        result = dict(schema="nbreq-f5-memory-case-v1", valid=True, client=client_ready,
                      protocol=protocol, phases=phases, final=done, fixture_ready=ready,
                      fixture_stats=stats, fixture_process=fixture_observation.result(),
                      sample_interval_seconds=interval)
    except BaseException as error:
        failure = error
    finally:
        cleanup = []
        for child in reversed(children):
            try:
                cleanup.append(child.close())
            except Exception as error:
                failure = failure or error
                cleanup.append(dict(pid=child.process.pid, cleanup_error=str(error)))
        (folder / "events.json").write_text(json.dumps(records, indent=2), encoding="utf-8")
        (folder / "cleanup.json").write_text(json.dumps(cleanup, indent=2), encoding="utf-8")
    if failure:
        (folder / "failure.txt").write_text(repr(failure), encoding="utf-8")
        raise failure
    result["cleanup"] = cleanup
    (folder / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    return result


def host_info():
    value = dict(platform=platform.platform(), machine=platform.machine(), python=sys.version,
                 cpu_count=os.cpu_count(), utc=datetime.datetime.now(datetime.timezone.utc).isoformat())
    if sys.platform.startswith("linux"):
        value["meminfo"] = Path("/proc/meminfo").read_text()
        value["cpuinfo"] = Path("/proc/cpuinfo").read_text().split("\n\n")[0]
    elif sys.platform == "darwin":
        value["hardware"] = subprocess.check_output(["sysctl", "hw.memsize", "hw.model", "machdep.cpu.brand_string"], text=True)
    else:
        value["processor"] = platform.processor()
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plain", required=True, type=Path)
    parser.add_argument("--meter", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source", required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--smoke", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.repeats <= 5:
        parser.error("repeats must be 1..5")
    args.output.mkdir(parents=True, exist_ok=False)
    provenance = dict(host=host_info(), source=args.source, binaries={
        name: dict(path=str(path.resolve()), sha256=hashlib.sha256(path.read_bytes()).hexdigest())
        for name, path in [("plain", args.plain), ("meter", args.meter)]})
    (args.output / "provenance.json").write_text(json.dumps(provenance, indent=2), encoding="utf-8")
    matrix = list(itertools.product(["http", "https"], [1, 16, 32], [1024, 8192, 51200]))
    workloads = [(protocol, connections, size, False) for protocol, connections, size in matrix]
    workloads += [(protocol, 32, 51200, True) for protocol in ("http", "https")]
    if args.smoke:
        workloads = [("http", 1, 1024, False), ("https", 32, 51200, True)]
    results = []
    for repetition in range(args.repeats):
        for protocol, connections, size, extended in workloads:
            modes = [("plain", args.plain), ("meter", args.meter)]
            if repetition % 2:
                modes.reverse()
            for name, binary in modes:
                case = "%s-c%d-b%d-%s-%s-r%d" % (protocol, connections, size,
                                                  "extended" if extended else "normal", name, repetition + 1)
                result = run_case(args.plain.resolve(), binary.resolve(), args.output / case, args.source,
                                  protocol, connections, size, max(16, 256 // connections), extended)
                if result["client"]["instrumented"] != (name == "meter"):
                    raise ValueError("plain/meter binary feature mismatch")
                result["case"] = case
                result["repetition"] = repetition + 1
                results.append(result)
                print("PASS " + case, flush=True)
    (args.output / "results.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
    print("M1_MEASUREMENTS_PASS cases=%d" % len(results), flush=True)


if __name__ == "__main__":
    main()
