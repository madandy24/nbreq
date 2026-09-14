"""Validate completed R4 evidence and package its raw logs without build trees or private keys."""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile

SOURCE = 'e78b73b18d4c3b969f7e022a705b8c84146dab44d39a2d14cbe1893242936914'


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def validate(folder, seconds):
    result = read(folder/'result.json')
    assert result['status'] == 'passed' and result['failure'] is None, str(folder)
    assert result['metadata']['source'] == SOURCE
    assert result['metadata']['requested_seconds'] == seconds
    assert result['metadata']['client']['debug_assertions'] is False
    assert result['final']['elapsed_s'] >= seconds
    required = {'exact_bytes', 'quiescent', 'bounds', 'terminals', 'joined_shutdown', 'live_stream_stopped'}
    assert set(result['final']['checks']) == required and all(result['final']['checks'].values())
    assert {item['label'] for item in result['cleanup']} == {'client', 'http', 'https', 'echo'}
    assert len(result['cleanup']) == 4
    assert all(item['exited'] and item['reader_joined'] and not item['forced'] and item['exit_code'] == 0
               for item in result['cleanup'])
    for item in result['metadata']['fixtures']:
        fixture = item['result']
        assert fixture['joined'] and fixture['socket_owners'] == 0
        assert fixture['stats']['bad_body'] == 0 and fixture['stats']['active_requests'] == 0
    return result


def pack(args):
    preflight, soak = args.preflight.resolve(), args.soak.resolve()
    assert read(preflight/'result.json')['status'] == 'passed'
    assert read(soak/'result.json')['status'] == 'passed'
    assert read(soak/'result.json')['source'] == SOURCE
    results = [validate(preflight/'smoke-default', 30), validate(preflight/'smoke-native', 180),
               validate(soak/'mixed', 14400)]
    checks = read(preflight/'checks/checks.json')
    assert len(checks) == 5
    for item in checks[:3]:
        assert item['failure'] is None and item['cleanup']['exited'] and item['cleanup']['reader_joined']
        assert not item['cleanup']['forced']
    assert all(item['passed'] for item in checks[3:])
    binaries = read(preflight/'result.json')['binaries']
    for label, binary in binaries.items():
        assert hashlib.sha256(Path(binary['path']).read_bytes()).hexdigest() == binary['sha256']
    assert results[-1]['metadata']['binary_sha256'] == binaries['default']['sha256']
    output = args.output.resolve()
    assert not output.exists(), 'do not overwrite an evidence archive'
    inputs = []
    for label, folder in [('preflight', preflight), ('soak', soak)]:
        for current, dirs, files in os.walk(folder):
            dirs[:] = [name for name in dirs if not name.startswith('build-') and name != '__pycache__']
            for name in sorted(files):
                path = Path(current)/name
                assert not path.is_symlink() and folder in path.resolve().parents
                inputs.append((path, label+'/'+path.relative_to(folder).as_posix()))
    manifest = dict(source=SOURCE, host=args.label, files={name: hashlib.sha256(path.read_bytes()).hexdigest()
                                                        for path, name in inputs})
    encoded = json.dumps(manifest, indent=2).encode('utf-8')
    with tarfile.open(output, 'w:gz', compresslevel=9) as archive:
        for path, name in inputs:
            archive.add(path, arcname=name, recursive=False)
        entry = tarfile.TarInfo('evidence-manifest.json')
        entry.size = len(encoded)
        archive.addfile(entry, io.BytesIO(encoded))
    summary = dict(host=args.label, source=SOURCE, archive=str(output), bytes=output.stat().st_size,
                   sha256=hashlib.sha256(output.read_bytes()).hexdigest(), files=len(inputs),
                   final=results[-1]['final'], process_summary=results[-1]['process_summary'])
    output.with_suffix('.json').write_text(json.dumps(summary, indent=2), encoding='utf-8')
    print(json.dumps(summary), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='mode', required=True)
    build = commands.add_parser('pack')
    build.add_argument('--preflight', type=Path, required=True)
    build.add_argument('--soak', type=Path, required=True)
    build.add_argument('--output', type=Path, required=True)
    build.add_argument('--label', required=True)
    export = commands.add_parser('emit')
    export.add_argument('--archive', type=Path, required=True)
    export.add_argument('--part', type=int, required=True)
    args = parser.parse_args()
    if args.mode == 'pack':
        pack(args)
    else:
        data = args.archive.read_bytes()
        size = 384*1024
        assert 0 <= args.part < (len(data)+size-1)//size
        chunk = data[args.part*size:(args.part+1)*size]
        print(json.dumps(dict(part=args.part, total_parts=(len(data)+size-1)//size,
                              archive_sha256=hashlib.sha256(data).hexdigest(),
                              chunk_sha256=hashlib.sha256(chunk).hexdigest(),
                              base64=base64.b64encode(chunk).decode('ascii'))), flush=True)


if __name__ == '__main__':
    main()
