"""Scoped, same-host R4 comparison of the common native HTTP API with registry nbreq 0.1.1."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'tools/release-soak'))
from host import command
from process_sample import Sampler


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def verify_inputs():
    path = ROOT/'r4-compare-source.json'
    manifest = read(path)
    for name, digest in manifest['files'].items():
        assert hashlib.sha256((ROOT/name).read_bytes()).hexdigest() == digest, name
    return hashlib.sha256(path.read_bytes()).hexdigest(), manifest


def observe(binary, out, label, version, identity, body, requests, instrumented):
    out.mkdir()
    argv = [str(binary), '--source-commit', identity, '--source-version', version,
            '--samples', '3', '--warmups', '32', '--requests-per-sample', str(requests),
            '--body-bytes', str(body)]
    sampler = None
    samples = []
    started = time.monotonic()
    with (out/'stdout.json').open('wb') as stdout, (out/'stderr.log').open('wb') as stderr:
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        try:
            sampler = Sampler(process.pid)
            with (out/'process.jsonl').open('w', encoding='utf-8') as log:
                while process.poll() is None:
                    if time.monotonic()-started > 180:
                        raise TimeoutError('comparison child exceeded its 180-second bound')
                    try:
                        value = sampler.sample()
                        record = dict(elapsed_s=time.monotonic()-started, values=value)
                        samples.append(record)
                        log.write(json.dumps(record)+'\n')
                    except (OSError, KeyError, IndexError, subprocess.SubprocessError):
                        if process.poll() is None:
                            raise
                    time.sleep(0.01)
            assert process.wait(timeout=5) == 0, label+' failed: inspect '+str(out/'stderr.log')
        finally:
            if process.poll() is None:
                process.kill()  # Exact owned observer PID; its fixture is in-process.
                process.wait(timeout=5)
            if sampler:
                sampler.close()
    inner = read(out/'stdout.json')
    assert inner['schema'] == 'nbreq-f5-observation-v1'
    assert inner['source']['commit'] == identity and inner['source']['package_version'] == version
    assert inner['checks']['no_leftover_process'] is None
    assert len(inner['checks']) == 8 and all(value for key, value in inner['checks'].items() if key != 'no_leftover_process')
    inner['checks']['no_leftover_process'] = True  # Successful wait above proves this owned process exited.
    assert inner['workload']['allocator_instrumented'] == instrumented
    assert inner['workload']['fixture_scope'] == 'in_process'
    assert inner['workload']['body_bytes'] == body and inner['workload']['requests_per_sample'] == requests
    assert inner['measures']['connections_reused']['median'] == 31+3*requests
    assert samples
    record = dict(label=label, command=argv, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  process_wall_s=time.monotonic()-started, exit_code=process.returncode, forced=False,
                  process_scope='observer and loopback fixture together; not client-only memory or idle CPU',
                  sampling='10ms nominal sampling; last sampled CPU can omit final shutdown work',
                  sampled_peak_rss=max(s['values']['rss'] for s in samples),
                  sampled_peak_private=max((s['values']['private'] for s in samples if s['values']['private'] is not None), default=None), inner=inner)
    (out/'result.json').write_text(json.dumps(record, indent=2), encoding='utf-8')
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['build', 'run'])
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    identity, source = verify_inputs()
    if args.mode == 'build':
        out.mkdir(parents=True, exist_ok=False)
        (out/'bin').mkdir()
        env = dict(os.environ)
        env.pop('CARGO_BUILD_TARGET', None)
        env['PATH'] = str(Path.home()/'.cargo/bin')+os.pathsep+env.get('PATH','')
        cargo = str(Path.home()/'.cargo/bin'/('cargo.exe' if os.name == 'nt' else 'cargo'))
        command([cargo,'--version'], ROOT, out, 'cargo-version', env, 30)
        command([str(Path(cargo).with_name('rustc.exe' if os.name == 'nt' else 'rustc')),'-vV'],
                ROOT, out, 'rustc-version', env, 30)
        binaries = {}
        for version, manifest in [('current','tools/f5-observe/Cargo.toml'), ('v011','tools/f5-observe/v011/Cargo.toml')]:
            env['CARGO_TARGET_DIR'] = str(out/('build-'+version))
            for kind, features in [('plain', []), ('alloc', ['--features','alloc-stats'])]:
                label = version+'-'+kind
                common = ['--locked','--manifest-path',manifest,'--no-default-features']+features
                command([cargo,'clippy']+common+['--all-targets','--','-D','warnings'],ROOT,out,'clippy-'+label,env)
                command([cargo,'build','--release']+common,ROOT,out,'build-'+label,env)
                name = 'nbreq-f5-observe'+('.exe' if os.name == 'nt' else '')
                binary = out/'bin'/(label+('.exe' if os.name == 'nt' else ''))
                shutil.copyfile(Path(env['CARGO_TARGET_DIR'])/'release'/name,binary)
                if os.name != 'nt':
                    binary.chmod(0o700)
                binaries[label] = dict(path=str(binary),sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
        (out/'build.json').write_text(json.dumps(dict(source=identity,binaries=binaries),indent=2),encoding='utf-8')
        print(json.dumps(dict(status='built',source=identity,out=str(out))),flush=True)
        return
    built = read(out/'build.json')
    assert built['source'] == identity
    for binary in built['binaries'].values():
        assert hashlib.sha256(Path(binary['path']).read_bytes()).hexdigest() == binary['sha256']
    records = []
    for kind in ['plain','alloc']:
        for repetition in range(3):
            sizes = [(1024,4096),(65536,4096),(1048576,256)]
            if repetition % 2:
                sizes.reverse()
            for body, requests in sizes:
                versions = ['current','v011'] if repetition % 2 == 0 else ['v011','current']
                for version in versions:
                    label = version+'-'+kind+'-'+str(body)+'-'+str(repetition)
                    origin = ('manifest:'+source['runtime_source']) if version == 'current' else source['registry_commit']
                    record = observe(Path(built['binaries'][version+'-'+kind]['path']),out/label,label,
                                     '0.2.0' if version == 'current' else '0.1.1',origin,body,requests,kind=='alloc')
                    records.append(dict(label=label,wall_ms=record['inner']['measures']['wall_ms']))
                    print(label+' passed',flush=True)
    (out/'result.json').write_text(json.dumps(dict(status='passed',source=identity,runs=len(records),records=records),indent=2),encoding='utf-8')


if __name__ == '__main__':
    main()
