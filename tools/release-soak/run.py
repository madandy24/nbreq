"""Bounded R4 supervisor: isolated fixtures, raw events, client-only process sampling and cleanup."""
import argparse
import collections
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import subprocess
import sys
import threading
import time

from process_sample import Sampler


class Child:
    def __init__(self, label, command, out):
        self.label = label
        self.messages = queue.Queue(maxsize=128)
        self.stdout = (out / (label + '.jsonl')).open('w', encoding='utf-8')
        self.stderr = (out / (label + '.stderr')).open('wb')
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.stderr, text=True, encoding='utf-8', bufsize=1)
        self.forced = False
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def _read(self):
        try:
            for line in self.process.stdout:
                self.stdout.write(line)
                self.stdout.flush()
                self.messages.put(json.loads(line))
        except Exception as error:
            self.messages.put({'reader_error': repr(error)})
        finally:
            self.messages.put(None)

    def receive(self, timeout):
        event = self.messages.get(timeout=timeout)
        if event is None:
            raise RuntimeError(self.label + ' exited before its required completion event')
        if 'reader_error' in event:
            raise RuntimeError(self.label + ': ' + event['reader_error'])
        return event

    def stop_fixture(self):
        if self.process.poll() is None:
            self.process.stdin.write('\n')
            self.process.stdin.flush()

    def close(self):
        # Only exact PIDs created by this controller are eligible for termination.
        # Pipe EOF can precede the OS reporting process exit; allow that ordinary exit
        # to finish before classifying cleanup as forced.
        if self.label == 'client' and self.process.poll() is None:
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                pass
        if self.process.poll() is None:
            if self.label != 'client':
                try:
                    self.stop_fixture()
                    self.process.wait(timeout=15)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            if self.process.poll() is None:
                self.forced = True
                self.process.terminate()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)
        self.thread.join(timeout=3)
        # Readers should have drained their bounded streams; a stuck reader is a failed cleanup.
        reader_joined = not self.thread.is_alive()
        if reader_joined:
            self.stdout.close()
        self.stderr.close()
        return dict(label=self.label, pid=self.process.pid, exit_code=self.process.returncode,
                    forced=self.forced, reader_joined=reader_joined, exited=self.process.poll() is not None)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=14400)
    parser.add_argument('--source', default='unfrozen-development')
    parser.add_argument('--watchdog', type=float, default=75)
    parser.add_argument('--corrupt', action='store_true')
    args = parser.parse_args(argv)
    assert 0 < args.seconds <= 86400 and 0 < args.watchdog <= 300
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    binary = str(args.binary.resolve())
    metadata = dict(schema='nbreq-r4-reliability-v1', source=args.source,
                    binary_sha256=hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                    started_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    host=platform.platform(), machine=platform.machine(), python=sys.version,
                    controller_pid=os.getpid(), requested_seconds=args.seconds,
                    process_scope='client only; HTTP/TLS/TCP fixtures are separate processes',
                    sampling='Windows working set/private/CPU; Linux proc RSS/private/CPU; macOS ps RSS/CPU, private unavailable',
                    interpretation='sampled process memory; no exact heap or cross-host performance claim')
    (out / 'metadata.json').write_text(json.dumps(metadata, indent=2), encoding='utf-8')
    children = []
    cleanup = []
    fixtures = []
    failure = None
    done = None
    sampler = None
    summary = dict(samples=0, max_rss=0, max_private=None)
    first = []
    last = collections.deque(maxlen=100)
    try:
        for label, extra in [('http', []), ('https', ['--tls', 'yes']), ('echo', ['--echo', 'yes'])]:
            command = [binary, 'fixture'] + extra
            if args.corrupt and label == 'http':
                command += ['--corrupt', 'yes']
            child = Child(label, command, out)
            children.append(child)
            ready = child.receive(20)
            assert ready['event'] == 'fixture_ready' and ready['pid'] == child.process.pid
            fixtures.append((child, ready))
        (out / 'root.der').write_bytes(bytes(fixtures[1][1]['root_der']))
        command = [binary, 'soak', '--plain', 'http://' + fixtures[0][1]['address'],
                   '--tls', 'https://' + fixtures[1][1]['address'], '--tcp', fixtures[2][1]['address'],
                   '--root', str(out / 'root.der'), '--seconds', str(args.seconds)]
        client = Child('client', command, out)
        children.append(client)
        metadata['children'] = [{'label': c.label, 'pid': c.process.pid} for c in children]
        (out / 'metadata.json').write_text(json.dumps(metadata, indent=2), encoding='utf-8')
        sampler = Sampler(client.process.pid)
        started = time.monotonic()
        previous_progress = started
        previous_clock = (time.time(), time.monotonic())
        next_sample = started
        cycle = 0
        saw_start = False
        with (out / 'process.jsonl').open('w', encoding='utf-8') as samples, \
             (out / 'supervisor.jsonl').open('w', encoding='utf-8') as audit:
            while done is None:
                now = time.monotonic()
                if now - previous_progress > args.watchdog or now - started > args.seconds + 120:
                    raise TimeoutError('client progress or whole-run bound expired')
                wall, mono = time.time(), time.monotonic()
                clock_gap = (wall - previous_clock[0]) - (mono - previous_clock[1])
                if abs(clock_gap) > 10:
                    audit.write(json.dumps(dict(event='clock_gap', seconds=clock_gap, elapsed_s=now-started))+'\n')
                    audit.flush()
                previous_clock = (wall, mono)
                if now >= next_sample and client.process.poll() is None:
                    try:
                        value = sampler.sample()
                        record = dict(elapsed_s=now-started, utc=time.time(), values=value)
                        samples.write(json.dumps(record)+'\n')
                        samples.flush()
                        summary['samples'] += 1
                        summary['max_rss'] = max(summary['max_rss'], value['rss'])
                        if value.get('private') is not None:
                            summary['max_private'] = max(summary['max_private'] or 0, value['private'])
                        if now-started >= 60:
                            if len(first) < 100:
                                first.append(value['rss'])
                            last.append(value['rss'])
                    except (OSError, subprocess.SubprocessError, IndexError, KeyError) as error:
                        audit.write(json.dumps(dict(event='sample_error', error=repr(error), elapsed_s=now-started))+'\n')
                        audit.flush()
                        if client.process.poll() is None:
                            raise
                    next_sample = now + 2
                try:
                    event = client.receive(0.2)
                except queue.Empty:
                    continue
                previous_progress = time.monotonic()
                kind = event.get('event')
                if kind == 'soak_start':
                    assert not saw_start and event['source'] == args.source
                    assert event['duration_s'] == args.seconds and event['private_root']
                    metadata['client'] = event
                    saw_start = True
                elif kind == 'cycle_begin':
                    assert saw_start and event['cycle'] == cycle + 1
                elif kind == 'cycle_end':
                    assert event['cycle'] == cycle + 1 and all(event['checks'].values())
                    cycle += 1
                    progress = dict(status='running', cycles=cycle, elapsed_s=event['elapsed_s'],
                                    metrics=event['metrics'], updated_utc_ms=event['utc_ms'])
                    (out / 'progress.json').write_text(json.dumps(progress, indent=2), encoding='utf-8')
                elif kind == 'public_network_pass':
                    pass
                elif kind == 'soak_end':
                    assert saw_start and event['cycles'] == cycle and all(event['checks'].values())
                    assert event['elapsed_s'] >= args.seconds
                    done = event
                else:
                    raise RuntimeError('unexpected client event ' + str(kind))
        assert client.process.wait(timeout=10) == 0
        stopped = []
        for child, ready in fixtures:
            child.stop_fixture()
            event = child.receive(20)
            assert event['event'] == 'fixture_stopped' and event['joined'] and event['socket_owners'] == 0
            assert event['stats']['bad_body'] == 0 and event['stats']['active_requests'] == 0
            if child.label != 'echo':
                assert event['stats']['high_active_requests'] >= 2, 'fixture missed required concurrency'
            assert child.process.wait(timeout=10) == 0
            stopped.append(dict(label=child.label, result=event))
        metadata['fixtures'] = stopped
    except BaseException as error:
        failure = repr(error)
    finally:
        if sampler:
            sampler.close()
        for child in reversed(children):
            try:
                cleanup.append(child.close())
            except BaseException as error:
                cleanup.append(dict(label=child.label, cleanup_error=repr(error)))
                failure = failure or repr(error)
    if any(c.get('forced') or not c.get('exited') or not c.get('reader_joined') for c in cleanup):
        failure = failure or 'forced or incomplete child cleanup'
    if first and last:
        summary.update(first_warm_rss_mean=sum(first)/len(first), last_warm_rss_mean=sum(last)/len(last))
    result = dict(status='failed' if failure else 'passed', failure=failure, metadata=metadata,
                  final=done, process_summary=summary, cleanup=cleanup)
    (out / 'result.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    print(json.dumps(dict(status=result['status'], failure=failure, out=str(out), summary=summary)), flush=True)
    return 1 if failure else 0


if __name__ == '__main__':
    raise SystemExit(main())
