"""Run built A/B/C examples with local fixtures; live HTTPS/DNS are explicit opt-ins."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time


HTTP = [
    ('A01-http-blocking-get', 'GET', 1, None),
    ('A02-http-blocking-post', 'POST', 1, b'hello from nbreq'),
    ('A03-http-full-get', 'GET', 2, None),
    ('A04-http-full-post', 'POST', 1, b'{"message":"hello from nbreq"}'),
    ('A05-http-nonblocking', 'GET', 2, None),
    ('A06-http-callbacks', 'GET', 1, None),
    ('A08-http-manual', 'GET', 1, None),
    ('A09-http-streaming', 'POST', 1, b'hello from nbreq'),
    ('A10-http-memory-limits', 'GET', 1, None),
    ('A11-http-owner-lifecycle', 'GET', 1, None),
]
DNS = ['B01-dns-blocking', 'B02-dns-nonblocking', 'B03-dns-manual']
TCP = ['C01-tcp-blocking', 'C02-tcp-nonblocking', 'C03-tcp-manual']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--live-dns', help='Run all three DNS examples against this public hostname')
    parser.add_argument('--live-https', help='Also run A01 against this platform-trusted HTTPS URL')
    args = parser.parse_args()
    if args.live_https and not args.live_https.startswith('https://'):
        parser.error('--live-https must be an HTTPS URL')
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    binaries = args.bin_dir.resolve()
    suffix = '.exe' if os.name == 'nt' else ''
    results = []
    requests = queue.Queue()
    errors = queue.Queue()

    def run(label, name, arguments=(), expected='HTTP 200', success=True):
        binary = binaries / (name + suffix)
        command = [str(binary), *arguments]
        print(label, flush=True)
        started = time.monotonic()
        try:
            result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
            output, code = result.stdout, result.returncode
        except subprocess.TimeoutExpired as error:
            output, code = error.stdout or b'', None
        (out / (label + '.log')).write_bytes(output)
        passed = code is not None and (code == 0) == success and expected.encode() in output
        results.append(dict(label=label, command=command, exit_code=code, passed=passed,
                            elapsed_s=time.monotonic() - started,
                            binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest()))
        (out / 'results.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
        if not passed:
            raise RuntimeError(label + ' failed:\n' + output.decode('utf-8', errors='replace'))

    class Handler(http.server.BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(5)

        def log_message(self, *args):
            pass

        def read_body(self):
            if self.headers.get('Transfer-Encoding') == 'chunked':
                body = bytearray()
                while True:
                    line = self.rfile.readline(128)
                    size = int(line.strip(), 16)
                    if size == 0:
                        assert self.rfile.readline(128) == b'\r\n'
                        return bytes(body)
                    assert 0 < size <= 65536 - len(body)
                    chunk = self.rfile.read(size)
                    assert len(chunk) == size and self.rfile.read(2) == b'\r\n'
                    body.extend(chunk)
            length = int(self.headers.get('Content-Length', '0'))
            assert 0 <= length <= 65536
            body = self.rfile.read(length)
            assert len(body) == length
            return body

        def respond(self):
            try:
                body = self.read_body()
                requests.put(dict(method=self.command, path=self.path, body=body,
                                  content_type=self.headers.get('Content-Type')))
                response = json.dumps(dict(method=self.command, data=body.decode())).encode()
                self.send_response(404 if self.path == '/missing' else 200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(1024 * 1024 + 1 if self.path == '/oversize' else len(response)))
                self.end_headers()
                if self.path != '/oversize':
                    self.wfile.write(response)
            except Exception as error:
                errors.put(repr(error))

        do_GET = respond
        do_POST = respond

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = False
    worker = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.05))
    worker.start()
    url = f'http://127.0.0.1:{server.server_port}'
    try:
        for name, method, count, body in HTTP:
            expected = 'EngineStopped (verified)' if name == 'A11-http-owner-lifecycle' else 'HTTP 200'
            run(name, name, [url + '/' + method.lower()], expected)
            seen = [requests.get(timeout=5) for _ in range(count)]
            assert all(item['method'] == method and item['body'] == (body or b'') for item in seen), seen
            if name == 'A04-http-full-post':
                assert seen[0]['content_type'] == 'application/json', seen
            if name == 'A09-http-streaming':
                assert 'streamed ' in (out / (name + '.log')).read_text(), 'must consume through EOF'
            assert requests.empty(), 'unexpected extra request'
        run('http-status-is-response', 'A03-http-full-get', [url + '/missing'], 'HTTP 404')
        assert all(requests.get(timeout=5)['path'] == '/missing' for _ in range(2))
        run('response-limit-failure', 'A10-http-memory-limits', [url + '/oversize'],
            'ResponseBodyBytes', success=False)
        assert requests.get(timeout=5)['path'] == '/oversize'
    finally:
        server.shutdown()
        server.server_close()  # Joins bounded request workers.
        worker.join()
    assert errors.empty(), list(errors.queue)
    assert requests.empty(), 'unexpected remaining request'
    run('A07-http-cancel', 'A07-http-cancel', expected='completion: Cancelled (verified)')
    for name in TCP:
        run(name, name, expected='echoed 17 bytes and received EOF')
    if args.live_dns:
        for name in DNS:
            run('live-' + name, name, [args.live_dns], 'DNS Answer:')
    if args.live_https:
        run('live-https', 'A01-http-blocking-get', [args.live_https])
    summary = dict(status='passed', executions=len(results), local_only=not (args.live_dns or args.live_https),
                   dns='live system configuration' if args.live_dns else 'not executed; build separately',
                   live_dns=args.live_dns, live_https=args.live_https)
    (out / 'summary.json').write_text(json.dumps(summary, indent=2), encoding='utf-8')
    print(f'Example checks passed: {len(results)} executions', flush=True)


if __name__ == '__main__':
    main()
