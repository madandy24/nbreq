"""Check 0.2 candidates with registry helpers, or the published release with no overrides."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--mode', choices=['candidate', 'published'], required=True)
parser.add_argument('--package', type=Path, help='Exact root candidate archive; candidate mode only')
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--toolchains', nargs='+', default=['stable', '1.85.0'])
args = parser.parse_args()
if (args.mode == 'candidate') != (args.package is not None):
    parser.error('Only candidate mode requires --package')
source = Path(__file__).resolve().parent
out = args.out.resolve()
out.mkdir(parents=True, exist_ok=False)
work = Path(tempfile.mkdtemp(prefix='nbreq-020-registry-')).resolve()
env = dict(os.environ, CARGO_TARGET_DIR=str(out / 'build'))
env.pop('CARGO_BUILD_TARGET', None)
steps = []
cases = []
registry = 'registry+https://github.com/rust-lang/crates.io-index'
helper_hashes = {
    'nbreq-darwin': ('0.1.0', '31d7a69844b990331f85dc497f14d0b6a08f8ceaee402e7c6aa19919f3e3c839'),
    'nbreq-winpoll': ('0.1.1', '9e4fc64245514209beb6e3feccb4b980e1c259546a9e07543e3529e1d7053696'),
}


def run(label, command, cwd, expected=0, contains=None):
    print(label, flush=True)
    started = time.monotonic()
    result = subprocess.run(command, cwd=cwd, env=env, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=900)
    (out / (label + '.log')).write_bytes(result.stdout)
    passed = result.returncode == expected and (contains is None or contains.encode() in result.stdout)
    steps.append(dict(label=label, command=command, exit_code=result.returncode, passed=passed,
                      elapsed_s=time.monotonic() - started))
    (out / 'steps.json').write_text(json.dumps(steps, indent=2), encoding='utf-8')
    if not passed:
        print(result.stdout.decode('utf-8', errors='replace')[-5000:], flush=True)
        raise RuntimeError(label + ' failed; retain this directory')
    return result.stdout


if args.mode == 'published':
    request = urllib.request.Request('https://crates.io/api/v1/crates/nbreq/0.2.0/download',
                                    headers={'User-Agent': 'nbreq-020-registry-check'})
    with urllib.request.urlopen(request, timeout=60) as response:
        payload = response.read()
    package = out / 'nbreq-0.2.0.crate'
    package.write_bytes(payload)
else:
    package = args.package.resolve()
package_hash = hashlib.sha256(package.read_bytes()).hexdigest()
with tarfile.open(package) as archive:
    seen = set()
    for member in archive.getmembers():
        name = PurePosixPath(member.name)
        assert member.isfile() and not name.is_absolute() and '..' not in name.parts
        assert all('\\' not in part and ':' not in part for part in name.parts)
        assert name.parts[0] == 'nbreq-0.2.0' and member.name not in seen
        seen.add(member.name)
        destination = work.joinpath(*name.parts).resolve()
        assert destination.is_relative_to(work)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(archive.extractfile(member).read())
root = work / 'nbreq-0.2.0'
manifest = tomllib.loads((root / 'Cargo.toml').read_text())
assert manifest['package']['name'] == 'nbreq' and manifest['package']['version'] == '0.2.0'
assert 'patch' not in manifest
for target, helper in [('cfg(windows)', 'nbreq-winpoll'), ('cfg(target_os = "macos")', 'nbreq-darwin')]:
    assert 'path' not in manifest['target'][target]['dependencies'][helper]
inputs = dict(mode=args.mode, archive_sha256=package_hash,
              package_vcs=json.loads((root / '.cargo_vcs_info.json').read_text()),
              platform=platform.platform(), machine=platform.machine(), toolchains=args.toolchains,
              temporary_workspace=str(work), registry_only=args.mode == 'published',
              support_from_registry=True, helper_hashes=helper_hashes,
              test_source_files={str(p.relative_to(source)): hashlib.sha256(p.read_bytes()).hexdigest()
                                 for folder in ['src', 'probes'] for p in (source / folder).rglob('*.rs')})
(out / 'inputs.json').write_text(json.dumps(inputs, indent=2), encoding='utf-8')
for toolchain in args.toolchains:
    cargo = ['rustup', 'run', toolchain, 'cargo']
    run(toolchain + '-rustc', ['rustup', 'run', toolchain, 'rustc', '-vV'], work)
    for case in ['fresh', 'mio-coexist']:
        label = toolchain + '-' + case
        consumer = work / label
        consumer.mkdir()
        shutil.copytree(source / 'src', consumer / 'src')
        shutil.copytree(source / 'probes', consumer / 'probes')
        text = (source / 'Cargo.toml').read_text()
        if case == 'mio-coexist':
            text = text.replace('[dependencies]', '[dependencies]\nmio = "=1.2.3"')
        if args.mode == 'candidate':
            text += '\n[patch.crates-io]\nnbreq = { path = ' + json.dumps(root.as_posix()) + ' }\n'
        (consumer / 'Cargo.toml').write_text(text, encoding='utf-8')
        run(label + '-resolve', cargo + ['generate-lockfile'], consumer)
        raw = run(label + '-metadata', cargo + ['metadata', '--locked', '--all-features', '--format-version', '1'], consumer)
        metadata = json.loads(raw[raw.index(b'{'):])
        lock = tomllib.loads((consumer / 'Cargo.lock').read_text())
        packages = {p['name']: p for p in metadata['packages'] if p['name'] in ['nbreq', *helper_hashes]}
        assert set(packages) == {'nbreq', *helper_hashes}
        assert packages['nbreq']['source'] == (registry if args.mode == 'published' else None)
        for name, (version, digest) in helper_hashes.items():
            assert packages[name]['source'] == registry and packages[name]['version'] == version
            matches = [p for p in lock['package'] if p['name'] == name]
            assert len(matches) == 1 and matches[0]['source'] == registry and matches[0]['checksum'] == digest
        if args.mode == 'published':
            entry = next(p for p in lock['package'] if p['name'] == 'nbreq')
            assert entry['source'] == registry and entry['checksum'] == package_hash
        case_out = out / label
        case_out.mkdir()
        for filename in ['Cargo.lock', 'Cargo.toml']:
            shutil.copy2(consumer / filename, case_out / filename)
        for mode, features in [('default', []), ('native', ['--no-default-features', '--features', 'native,v020']),
                               ('minimal', ['--no-default-features', '--features', 'v020']),
                               ('test-support', ['--features', 'test-support'])]:
            run(label + '-' + mode, cargo + ['test', '--locked', '--lib', *features, '--', '--test-threads=1'],
                consumer, contains='test result: ok.')
        run(label + '-resolver-absent', cargo + ['check', '--locked', '--bin', 'resolver_probe',
            '--no-default-features', '--features', 'native,v020'], consumer, 101, 'unresolved import')
        run(label + '-testing-absent', cargo + ['check', '--locked', '--bin', 'testing_probe'], consumer, 101, 'unresolved import')
        cases.append(dict(label=label, registry_only=args.mode == 'published', support_from_registry=True,
                          packages={name: dict(version=p['version'], source=p['source']) for name, p in packages.items()},
                          lock_sha256=hashlib.sha256((consumer / 'Cargo.lock').read_bytes()).hexdigest()))
run('build-examples', ['rustup', 'run', args.toolchains[0], 'cargo', 'build', '--locked', '--examples',
                      '--manifest-path', str(root / 'Cargo.toml')], work)
run('examples', [sys.executable, str(source / 'check_examples.py'), '--bin-dir', str(out / 'build/debug/examples'),
                 '--out', str(out / 'examples')], work, contains='Example checks passed: 16')
(out / 'result.json').write_text(json.dumps(dict(status='passed', steps=len(steps), cases=cases,
    registry_only=args.mode == 'published', support_from_registry=True, archive_sha256=package_hash), indent=2), encoding='utf-8')
print('Registry consumer checks passed (' + args.mode + ')', flush=True)
