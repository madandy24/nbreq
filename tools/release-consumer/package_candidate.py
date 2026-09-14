"""Package a clean candidate into a fresh directory and inventory exact archives; never publish."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import time
import tomllib
from urllib.parse import unquote, urlsplit

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--out',type=Path,required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
out = args.out.resolve()
assert not subprocess.check_output(['git','status','--porcelain'],cwd=root).strip(), 'Candidate must be clean'
out.mkdir(parents=True,exist_ok=False)
commit = subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
env = dict(os.environ,CARGO_TARGET_DIR=str(out/'build'))
env.pop('CARGO_BUILD_TARGET',None)
packages = {}
steps = []
for name, version in [('nbreq-darwin','0.1.0'),('nbreq-winpoll','0.1.1'),('nbreq','0.2.0')]:
    command = ['cargo','package','--locked','--offline','-p',name]
    if name == 'nbreq':
        for helper, folder in [('nbreq-darwin','support/darwin'),('nbreq-winpoll','support/winpoll')]:
            command += ['--config','patch.crates-io.'+helper+'.path='+json.dumps((root/folder).as_posix())]
    print(name,flush=True)
    started = time.monotonic()
    result = subprocess.run(command,cwd=root,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=900)
    (out/(name+'.log')).write_bytes(result.stdout)
    steps.append(dict(command=command,exit_code=result.returncode,elapsed_s=time.monotonic()-started))
    (out/'steps.json').write_text(json.dumps(steps,indent=2),encoding='utf-8')
    if result.returncode:
        print(result.stdout.decode('utf-8',errors='replace')[-5000:],flush=True)
        raise RuntimeError('Packaging failed; retain this fresh directory')
    path = out/'build/package'/(name+'-'+version+'.crate')
    prefix = name+'-'+version+'/'
    with tarfile.open(path) as archive:
        entries = archive.getmembers()
        names = [entry.name for entry in entries]
        assert len(set(names)) == len(names)
        assert all(e.isfile() and e.name.startswith(prefix) and '..' not in PurePosixPath(e.name).parts for e in entries)
        vcs = json.load(archive.extractfile(prefix+'.cargo_vcs_info.json'))
        assert vcs['git']['sha1'] == commit and not vcs['git'].get('dirty',False)
        manifest = tomllib.loads(archive.extractfile(prefix+'Cargo.toml').read().decode())
        assert manifest['package']['name'] == name and manifest['package']['version'] == version
        links = []
        for entry in entries:
            if not entry.name.endswith('.md'):
                continue
            document = archive.extractfile(entry).read().decode()
            for destination in re.findall(r'\]\(([^\s)]+)\)', document):
                url = urlsplit(destination)
                if url.scheme or url.netloc or not url.path:
                    continue
                # Resolve relative documentation paths within this exact archive.
                parts = list(PurePosixPath(entry.name).parent.parts)
                for part in PurePosixPath(unquote(url.path)).parts:
                    if part == '..':
                        assert len(parts) > 1, 'Link escapes package: '+destination
                        parts.pop()
                    elif part != '.':
                        parts.append(part)
                target = '/'.join(parts)
                assert target in names, 'Missing packaged link target: '+target
                links.append(dict(document=entry.name[len(prefix):],destination=destination,
                                  packaged_target=target[len(prefix):]))
        if name == 'nbreq':
            assert not any(n.startswith(prefix+p) for n in names for p in ['thoughts/','tools/','target/'])
            for doc in ['README.md','SECURITY.md','docs/getting-started.md','docs/migrating-to-0.2.md']:
                text = archive.extractfile(prefix+doc).read().decode()
                assert 'currently unreleased' not in text and 'latest published line is 0.1.1' not in text
            for key, helper, minimum in [('cfg(windows)','nbreq-winpoll','0.1.1'),('cfg(target_os = "macos")','nbreq-darwin','0.1.0')]:
                dependency = manifest['target'][key]['dependencies'][helper]
                assert dependency['version'] == minimum and 'path' not in dependency
        hashes = {e.name[len(prefix):]:hashlib.sha256(archive.extractfile(e).read()).hexdigest() for e in entries}
    packages[path.name] = dict(path=str(path),bytes=path.stat().st_size,sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                                commit=commit,files=hashes,relative_links=links)
    (out/'packages.json').write_text(json.dumps(dict(source_commit=commit,
        registry_only=False,support_override='Root package uses explicit current local Darwin/winpoll paths',packages=packages),indent=2),encoding='utf-8')
assert not subprocess.check_output(['git','status','--porcelain'],cwd=root).strip()
(out/'result.json').write_text(json.dumps(dict(status='passed',source_commit=commit,packages=list(packages),
    registry_only=False),indent=2),encoding='utf-8')
