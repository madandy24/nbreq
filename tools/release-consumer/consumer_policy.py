"""Explicit consumer-lock compatibility policy; never edits a library manifest."""
import hashlib
import json
from pathlib import Path
import re
import shutil
import tomllib


def lock_identity(path):
    data = path.read_bytes()
    packages = tomllib.loads(data.decode('utf-8'))['package']
    return dict(sha256=hashlib.sha256(data).hexdigest(),
                packages=[{key: p[key] for key in ('name', 'version', 'source', 'checksum') if key in p}
                          for p in packages])


def select_consumer_lock(run, label, cargo, toolchain, consumer, evidence, network=()):
    """Preserve fresh resolution, select the documented older-compiler graph, and record both."""
    consumer, evidence = Path(consumer), Path(evidence)
    evidence.mkdir(parents=True, exist_ok=True)
    raw = run(label+'-policy-rustc', ['rustup', 'run', toolchain, 'rustc', '-vV'], consumer)
    rustc = raw.decode('utf-8')
    match = re.search(r'^release: (\d+)\.(\d+)\.(\d+)(?:-\S+)?\r?$', rustc, re.MULTILINE)
    if not match:
        raise ValueError('Cannot determine selected rustc release; no lock policy applied')
    version = tuple(map(int, match.groups()))
    older = version < (1, 87, 0)
    lock = consumer/'Cargo.lock'
    shutil.copy2(lock, evidence/'Cargo.initial.lock')
    initial = lock_identity(lock)
    command = [*cargo, 'update', *network, '-p', 'yoke-derive', '--precise', '0.8.2'] if older else None
    record = dict(route='compatibility-selected' if older else 'fresh', rustc=rustc,
                  policy='Rust <1.87: consumer-only yoke-derive 0.8.2; Rust >=1.87: unchanged fresh graph',
                  initial=initial, command=command, selected=None, status='selecting')
    policy_path = evidence/'consumer-policy.json'
    policy_path.write_text(json.dumps(record, indent=2), encoding='utf-8')
    print(label+': '+record['route'], flush=True)
    if command:
        run(label+'-compatibility-lock', command, consumer)
    selected = lock_identity(lock)
    if older:
        yoke = [p for p in selected['packages'] if p['name'] == 'yoke-derive']
        assert len(yoke) == 1 and yoke[0]['version'] == '0.8.2'
    else:
        assert initial['sha256'] == selected['sha256'], 'Fresh route changed its lock'
    if lock.resolve() != (evidence/'Cargo.lock').resolve():
        shutil.copy2(lock, evidence/'Cargo.lock')
    before = {json.dumps(p, sort_keys=True) for p in initial['packages']}
    after = {json.dumps(p, sort_keys=True) for p in selected['packages']}
    record.update(selected=selected, status='selected',
                  removed=[json.loads(p) for p in sorted(before-after)],
                  added=[json.loads(p) for p in sorted(after-before)])
    # The targeted downgrade may add/remove synstructure 0.13.2; unrelated graph churn is a failure.
    changed_names = {p['name'] for p in record['removed']+record['added']}
    if changed_names - {'yoke-derive', 'synstructure'}:
        record['status'] = 'unexpected-lock-changes'
        policy_path.write_text(json.dumps(record, indent=2), encoding='utf-8')
        raise ValueError('Compatibility selection changed unrelated packages; inspect recorded locks')
    policy_path.write_text(json.dumps(record, indent=2), encoding='utf-8')
    return record
