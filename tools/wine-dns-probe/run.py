"""Bounded native/Wine execution of the DNS diagnostic and ordinary NBReq lifecycle probe."""
import argparse, http.server, json, os, pathlib, subprocess, threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=pathlib.Path, required=True)
parser.add_argument('--out', type=pathlib.Path, required=True)
parser.add_argument('--wine', help='Wine executable; omit for native Windows execution')
parser.add_argument('--prefix', type=pathlib.Path, help='Explicit private test prefix, required with --wine')
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
environment = dict(os.environ)
if args.wine:
    assert args.prefix and args.prefix.is_absolute(), 'Supply a private absolute Wine prefix'
    environment.update(WINEPREFIX=str(args.prefix), WINEARCH='win32', WINEDEBUG='-all')
command = ([args.wine] if args.wine else []) + [str(args.binary.resolve())]
results = []

def run(label, parameters):
    print(label, flush=True)
    with (args.out/(label+'.log')).open('wb') as log:
        result = subprocess.run(command+parameters, env=environment, stdout=log,
                                stderr=subprocess.STDOUT, timeout=45)
    results.append(dict(label=label, exit_code=result.returncode))
    (args.out/'results.json').write_text(json.dumps(results, indent=2))
    assert result.returncode == 0, label+' failed: inspect its log'

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'wine-dns-ok'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *arguments): pass

run('inspect', [])
run('fixed', ['--fixed'])
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.05))
thread.start()
try:
    run('ordinary-nbreq', ['http://127.0.0.1:{}/'.format(server.server_port)])
finally:
    server.shutdown()
    server.server_close()
    thread.join()
print(json.dumps(dict(status='passed', wine=args.wine, prefix=str(args.prefix), results=results)), flush=True)
