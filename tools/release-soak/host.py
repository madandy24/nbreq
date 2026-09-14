"""R4 host jobs: explicit source verification, bounded build gates and retained soak results."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from run import main as run_soak
from check import main as check_harness


def write(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2), encoding='utf-8')
    temporary.replace(path)


def command(argv, cwd, out, label, env, timeout=1200):
    print(label, flush=True)
    with (out / (label+'.log')).open('wb') as log:
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=os.name != 'nt')
        write(out / 'command.json', dict(label=label, pid=process.pid, command=argv,
                                        timeout_seconds=timeout))
        try:
            code = process.wait(timeout=timeout)
        except BaseException:
            # Only this freshly created command tree is eligible for forced cleanup.
            if os.name == 'nt':
                subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'],
                               stdout=log, stderr=subprocess.STDOUT, timeout=15)
            else:
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=15)
            raise
    write(out / (label+'.result.json'), dict(command=argv, exit_code=code))
    if code != 0:
        raise RuntimeError(label+' failed with exit '+str(code))


def verify_source(root):
    manifest = root / 'r4-source.json'
    identity = hashlib.sha256(manifest.read_bytes()).hexdigest()
    for name, expected in json.loads(manifest.read_text(encoding='utf-8'))['files'].items():
        path = (root / name).resolve()
        assert root in path.parents, name
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            raise RuntimeError('source mismatch: '+name)
    return identity


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['launch', 'work'])
    parser.add_argument('--phase', choices=['preflight', 'soak'], required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--preflight', type=Path)
    parser.add_argument('--target')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    out = args.out.resolve()
    if args.mode == 'launch':
        out.mkdir(parents=True, exist_ok=False)
        argv = [sys.executable, str(Path(__file__).resolve()), 'work', '--phase', args.phase,
                '--out', str(out)]
        if args.preflight:
            argv += ['--preflight', str(args.preflight.resolve())]
        if args.target:
            argv += ['--target', args.target]
        with (out/'job.log').open('wb') as log:
            child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                     start_new_session=os.name != 'nt', close_fds=True)
        write(out/'launch.json', dict(pid=child.pid, command=argv, phase=args.phase))
        print(json.dumps(dict(launched=True, pid=child.pid, out=str(out))), flush=True)
        return 0
    started = time.time()
    source = None
    failure = None
    binaries = {}
    try:
        source = verify_source(root)
        write(out/'status.json', dict(status='running', phase=args.phase, pid=os.getpid(),
                                      source=source, started_utc=datetime.datetime.now(datetime.timezone.utc).isoformat()))
        if args.phase == 'preflight':
            env = dict(os.environ)
            env['PATH'] = str(Path.home()/'.cargo'/'bin') + os.pathsep + env.get('PATH', '')
            env['NBREQ_R4_SOURCE'] = source
            cargo = str(Path.home()/'.cargo'/'bin'/('cargo.exe' if os.name == 'nt' else 'cargo'))
            command([cargo, '--version'], root, out, 'cargo-version', env, 30)
            command([str(Path(cargo).with_name('rustc.exe' if os.name == 'nt' else 'rustc')), '-vV'],
                    root, out, 'rustc-version', env, 30)
            env['CARGO_TARGET_DIR'] = str(out/'build-tests')
            command([cargo, 'run', '--locked', '--manifest-path', 'tools/xtask/Cargo.toml', '--', 'verify'],
                    root, out, 'verify', env)
            for label, features in [('default', []), ('native', ['--no-default-features'])]:
                target = ['--target', args.target] if args.target else []
                env['CARGO_TARGET_DIR'] = str(out/('build-'+label))
                common = ['--locked', '--manifest-path', 'tools/release-soak/Cargo.toml'] + features + target
                command([cargo, 'clippy']+common+['--all-targets', '--', '-D', 'warnings'],
                        root, out, 'clippy-'+label, env)
                command([cargo, 'build', '--release']+common, root, out, 'build-'+label, env)
                binary = Path(env['CARGO_TARGET_DIR'])
                if args.target:
                    binary /= args.target
                binary = binary/'release'/('nbreq-release-soak.exe' if os.name == 'nt' else 'nbreq-release-soak')
                binaries[label] = dict(path=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
            # Keep ownership of all fixture processes in this bounded supervisor.
            previous = sys.argv
            try:
                sys.argv = ['check.py', '--binary', binaries['default']['path'], '--out', str(out/'checks'), '--source', source]
                check_harness()
            finally:
                sys.argv = previous
            for label, seconds in [('default', 30), ('native', 180)]:
                if run_soak(['--binary', binaries[label]['path'], '--out', str(out/('smoke-'+label)),
                             '--seconds', str(seconds), '--source', source]) != 0:
                    raise RuntimeError(label+' rehearsal failed')
        else:
            assert args.preflight is not None, 'soak requires a passed preflight'
            preflight = json.loads((args.preflight/'result.json').read_text(encoding='utf-8'))
            assert preflight['status'] == 'passed' and preflight['source'] == source
            binaries = preflight['binaries']
            binary = Path(binaries['default']['path'])
            assert hashlib.sha256(binary.read_bytes()).hexdigest() == binaries['default']['sha256']
            if run_soak(['--binary', str(binary), '--out', str(out/'mixed'), '--seconds', '14400',
                         '--source', source]) != 0:
                raise RuntimeError('mixed soak failed')
    except BaseException as error:
        failure = repr(error)
    result = dict(status='failed' if failure else 'passed', phase=args.phase, failure=failure,
                  source=source, binaries=binaries, elapsed_seconds=time.time()-started, pid=os.getpid())
    write(out/'result.json', result)
    write(out/'status.json', result)
    print(json.dumps(result), flush=True)
    return 1 if failure else 0


if __name__ == '__main__':
    raise SystemExit(main())
