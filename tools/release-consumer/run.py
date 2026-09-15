"""Exercise frozen .crate files from independent temporary Cargo workspaces; never publish."""
from pathlib import Path, PurePosixPath
import argparse, hashlib, json, os, shutil, subprocess, sys, tarfile, tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--package', type=Path, required=True)
parser.add_argument('--darwin-package', type=Path, required=True)
parser.add_argument('--winpoll-package', type=Path, required=True)
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--offline', action='store_true')
parser.add_argument('--toolchains', nargs='+', default=['stable', '1.85.0'])
parser.add_argument('--live-dns', help='Optional public hostname for the packaged resolver example')
parser.add_argument('--live-https', help='Optional platform-trusted URL for the packaged HTTPS example')
args = parser.parse_args()
out = args.out.resolve()
out.mkdir(parents=True, exist_ok=False)
source = Path(__file__).resolve().parent
shutil.copytree(source, out/'consumer-source', ignore=shutil.ignore_patterns('target','Cargo.lock','__pycache__'))
work = Path(tempfile.mkdtemp(prefix='nbreq-020-consumer-'))
env = dict(os.environ, CARGO_TARGET_DIR=str(out/'build'))
results = []

def unpack(path):
    assert path.name.endswith('.crate')
    with tarfile.open(path, 'r:gz') as archive:
        for member in archive.getmembers():
            name = PurePosixPath(member.name)
            assert not name.is_absolute() and '..' not in name.parts
            assert member.isfile(), 'Only regular source files are expected'
            destination = work.joinpath(*name.parts)
            destination.parent.mkdir(parents=True,exist_ok=True)
            destination.write_bytes(archive.extractfile(member).read())
    return work/path.name[:-len('.crate')]

def run(label, command, cwd, expected=0, contains=None, timeout=600):
    print(label, flush=True)
    try:
        result = subprocess.run(command,cwd=cwd,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=timeout)
        (out/(label+'.log')).write_bytes(result.stdout)
        passed = result.returncode == expected and (contains is None or contains.encode() in result.stdout)
        results.append(dict(label=label,command=command,cwd=str(cwd),exit_code=result.returncode,passed=passed))
        (out/'results.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
        if not passed:
            print(result.stdout.decode('utf-8',errors='replace')[-5000:],flush=True)
            raise RuntimeError(label+' failed')
    except subprocess.TimeoutExpired as error:
        (out/(label+'.log')).write_bytes(error.stdout or b'')
        raise

main_package = args.package.resolve()
darwin_package = args.darwin_package.resolve()
winpoll_package = args.winpoll_package.resolve()
package = unpack(main_package)
darwin = unpack(darwin_package)
winpoll = unpack(winpoll_package)
consumer = work/'consumer'
shutil.copytree(out/'consumer-source',consumer)
patch = ['--config', 'patch.crates-io.nbreq.path='+json.dumps(package.as_posix()),
         '--config', 'patch.crates-io.nbreq-darwin.path='+json.dumps(darwin.as_posix()),
         '--config', 'patch.crates-io.nbreq-winpoll.path='+json.dumps(winpoll.as_posix())]
network = ['--offline'] if args.offline else []
metadata = dict(temporary_workspace=str(work),registry_gate_closed=False,
               support_override='Explicit unpacked Darwin/winpoll archives; not registry-only proof',
               dependency_resolution='fresh cached index' if args.offline else 'fresh online index',
               packages={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in (main_package,darwin_package,winpoll_package)})
(out/'inputs.json').write_text(json.dumps(metadata,indent=2),encoding='utf-8')
run('resolve-consumer',['cargo',*patch,'generate-lockfile',*network],consumer)
shutil.copy2(consumer/'Cargo.lock',out/'consumer-Cargo.lock')
for toolchain in args.toolchains:
    cargo = ['rustup','run',toolchain,'cargo',*patch]
    for label, features in [('default',[]),('native-only',['--no-default-features','--features','native,v020']),
                            ('minimal',['--no-default-features','--features','v020']),
                            ('test-support',['--features','test-support'])]:
        run(toolchain+'-'+label,[*cargo,'test','--locked',*network,'--lib',*features,'--','--test-threads=1'],consumer,contains='test result: ok.')
    for label, probe, features, expected in [
            ('resolver-present','resolver_probe',[],0),
            ('resolver-absent','resolver_probe',['--no-default-features','--features','native,v020'],101),
            ('testing-present','testing_probe',['--features','test-support'],0),
            ('testing-absent','testing_probe',[],101)]:
        run(toolchain+'-'+label,[*cargo,'check','--locked',*network,'--bin',probe,*features],consumer,expected,
            'unresolved import' if expected else None)

# Freeze the same ordinary HTTP test source against the actual registry 0.1.1 dependency.
legacy = work/'consumer-011'
shutil.copytree(out/'consumer-source',legacy)
manifest = (legacy/'Cargo.toml').read_text()
manifest = manifest.replace('default = ["native", "resolver", "v020"]','default = ["native"]')
manifest = manifest.replace('resolver = ["native", "nbreq/resolver"]','resolver = []')
manifest = manifest.replace('test-support = ["nbreq/test-support"]','test-support = []')
manifest = manifest.replace('version = "=0.2.0"','version = "=0.1.1"')
(legacy/'Cargo.toml').write_text(manifest,encoding='utf-8')
run('resolve-011',['cargo','generate-lockfile',*network],legacy)
shutil.copy2(legacy/'Cargo.lock',out/'legacy-Cargo.lock')
for toolchain in args.toolchains:
    run(toolchain+'-011-compat',['rustup','run',toolchain,'cargo','test','--locked',*network,'--lib','--','--test-threads=1'],legacy,contains='test result: ok. 3 passed')

run('build-package-examples',['rustup','run',args.toolchains[0],'cargo',*patch,'build','--manifest-path',str(package/'Cargo.toml'),'--locked',*network,'--examples'],consumer)
example_command = [sys.executable, str(consumer/'check_examples.py'),
                   '--bin-dir', str(out/'build/debug/examples'), '--out', str(out/'examples')]
if args.live_dns:
    example_command += ['--live-dns', args.live_dns]
if args.live_https:
    example_command += ['--live-https', args.live_https]
run('packaged-examples', example_command, consumer, contains='Example checks passed:', timeout=900)
metadata['status']='passed'
(out/'inputs.json').write_text(json.dumps(metadata,indent=2),encoding='utf-8')
print('External consumer checks passed; support registry-only gate remains open.',flush=True)
