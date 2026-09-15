"""Fresh external consumer checks for the pre-R5 dependency policy; local overrides, never publish."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--toolchains', nargs='+', default=['stable', '1.85.0'])
args = parser.parse_args()
source = Path(__file__).resolve().parent
root = source.parents[1]
out = args.out.resolve()
out.mkdir(parents=True, exist_ok=False)
records = []
env = dict(os.environ, CARGO_TARGET_DIR=str(out/'build'))
env.pop('CARGO_BUILD_TARGET', None)

def run(label, command, cwd, expected=0, contains=None):
    print(label, flush=True)
    started = time.monotonic()
    result = subprocess.run(command,cwd=cwd,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=900)
    (out/(label+'.log')).write_bytes(result.stdout)
    passed = result.returncode == expected and (contains is None or contains.encode() in result.stdout)
    records.append(dict(label=label,command=command,cwd=str(cwd),exit_code=result.returncode,
                        elapsed_s=time.monotonic()-started,passed=passed))
    (out/'steps.json').write_text(json.dumps(records,indent=2),encoding='utf-8')
    if not passed:
        print(result.stdout.decode('utf-8',errors='replace')[-5000:],flush=True)
        raise RuntimeError(label+' failed; preserve this run before investigation')

inputs = {p.relative_to(root).as_posix():hashlib.sha256(p.read_bytes()).hexdigest()
          for p in [root/'Cargo.toml',root/'Cargo.lock',root/'src/body_budget.rs',
                    root/'support/winpoll/Cargo.toml',root/'support/darwin/Cargo.toml']}
(out/'inputs.json').write_text(json.dumps(dict(files=inputs,
    commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
    platform=platform.platform(),machine=platform.machine(),toolchains=args.toolchains,
    graph='fresh online, independent consumer lock per toolchain/case',
    support='local root/Darwin/winpoll overrides; not registry-only acceptance'),indent=2),encoding='utf-8')
for toolchain in args.toolchains:
    run(toolchain+'-rustc',['rustup','run',toolchain,'rustc','-vV'],root)
    for case in ['fresh','mio-coexist']:
        label = toolchain+'-'+case
        consumer = out/label
        consumer.mkdir()
        shutil.copytree(source/'src',consumer/'src')
        shutil.copytree(source/'probes',consumer/'probes')
        manifest = (source/'Cargo.toml').read_text(encoding='utf-8')
        if case == 'mio-coexist':
            manifest = manifest.replace('[dependencies]','[dependencies]\nmio = "=1.2.3"')
        manifest += '\n[patch.crates-io]\n'
        for name, folder in [('nbreq',root),('nbreq-winpoll',root/'support/winpoll'),('nbreq-darwin',root/'support/darwin')]:
            manifest += name+' = { path = '+json.dumps(folder.as_posix())+' }\n'
        (consumer/'Cargo.toml').write_text(manifest,encoding='utf-8')
        cargo = ['rustup','run',toolchain,'cargo']
        run(label+'-resolve',cargo+['generate-lockfile'],consumer)
        run(label+'-metadata',cargo+['metadata','--locked','--format-version','1'],consumer)
        for mode, features in [('default',[]),('native',['--no-default-features','--features','native,v020']),
                               ('minimal',['--no-default-features','--features','v020']),
                               ('test-support',['--features','test-support'])]:
            run(label+'-'+mode,cargo+['test','--locked','--lib']+features+['--','--test-threads=1'],consumer,
                contains='test result: ok.')
        run(label+'-resolver-absent',cargo+['check','--locked','--bin','resolver_probe',
            '--no-default-features','--features','native,v020'],consumer,101,'unresolved import')
        run(label+'-testing-absent',cargo+['check','--locked','--bin','testing_probe'],consumer,101,'unresolved import')
        metadata_text = (out/(label+'-metadata.log')).read_text(encoding='utf-8')
        metadata = json.loads(metadata_text[metadata_text.index('{'):])
        versions = {p['name']:p['version'] for p in metadata['packages'] if p['name'] in
                    ['nbreq','nbreq-winpoll','nbreq-darwin','mio','rustls','url','icu_normalizer','windows-sys']}
        if case == 'mio-coexist':
            assert versions['mio'] == '1.2.3'
        (consumer/'versions.json').write_text(json.dumps(versions,indent=2),encoding='utf-8')
        print(label+' passed '+json.dumps(versions),flush=True)
(out/'result.json').write_text(json.dumps(dict(status='passed',steps=len(records),cases=2*len(args.toolchains),
    registry_only=False,source_files=inputs),indent=2),encoding='utf-8')
