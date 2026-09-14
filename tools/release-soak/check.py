"""Prove fixture churn, corruption detection and watchdog cleanup before accepting a soak."""
import argparse
import contextlib
import json
from pathlib import Path
import socket
import ssl

from run import Child, main as run_soak


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--source', default='unfrozen-development')
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    binary = str(args.binary.resolve())
    results = []
    for label, extra in [('http', []), ('https', ['--tls', 'yes']), ('echo', ['--echo', 'yes'])]:
        child = Child(label, [binary, 'fixture'] + extra, out)
        failure = None
        try:
            ready = child.receive(20)
            assert ready['event'] == 'fixture_ready'
            host, port = ready['address'].rsplit(':', 1)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            context.load_verify_locations(cadata=ssl.DER_cert_to_PEM_cert(bytes(ready['root_der'])))
            for _ in range(300):
                stream = socket.create_connection((host, int(port)), timeout=10)
                if label == 'https':
                    stream = context.wrap_socket(stream, server_hostname=host)
                with stream:
                    if label == 'echo':
                        sent = bytes(range(64))
                        stream.sendall(sent)
                        stream.shutdown(socket.SHUT_WR)
                        received = bytearray()
                        while True:
                            chunk = stream.recv(4096)
                            if not chunk:
                                break
                            received.extend(chunk)
                        assert received == sent
                    else:
                        stream.sendall(b'GET /body/1/0/0 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n')
                        head = bytearray()
                        while not head.endswith(b'\r\n\r\n'):
                            assert len(head) < 8192
                            byte = stream.recv(1)
                            assert byte
                            head.extend(byte)
                        assert head.startswith(b'HTTP/1.1 200') and b'Content-Length: 1\r\n' in head
                        assert stream.recv(1) == b'\0'
            child.stop_fixture()
            stopped = child.receive(20)
            assert stopped['joined'] and stopped['socket_owners'] == 0
            assert stopped['stats']['connections'] == 300 and stopped['stats']['requests'] == 300
            assert stopped['stats']['completed'] == 300 and stopped['stats']['bad_body'] == 0
            assert child.process.wait(timeout=10) == 0
        except BaseException as error:
            failure = repr(error)
        finally:
            cleanup = child.close()
            result = dict(check=label+'-300-connections', failure=failure, cleanup=cleanup)
            results.append(result)
            (out / 'checks.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
        assert failure is None and cleanup['exited'] and not cleanup['forced'] and cleanup['reader_joined'], result

    for label, extra, expected_text in [('corruption', ['--corrupt'], 'incorrect body'),
                                         ('watchdog', ['--watchdog', '0.1'], None)]:
        folder = out / label
        command = ['--binary', binary, '--out', str(folder), '--seconds', '60',
                   '--source', args.source] + extra
        # Run the bounded supervisor in-process so an outer subprocess timeout cannot
        # orphan its fixtures. Its finally block retains ownership of every child.
        with (out / (label+'.log')).open('w', encoding='utf-8') as log, contextlib.redirect_stdout(log):
            exit_code = run_soak(command)
        report = json.loads((folder / 'result.json').read_text())
        assert exit_code == 1 and report['status'] == 'failed'
        assert len(report['cleanup']) == 4
        assert all(item['exited'] and item['reader_joined'] for item in report['cleanup'])
        if expected_text:
            assert expected_text in (folder/'client.stderr').read_text()
            assert not any(item['forced'] for item in report['cleanup'])
        else:
            assert 'bound expired' in report['failure']
            assert any(item['label'] == 'client' and item['forced'] for item in report['cleanup'])
        results.append(dict(check=label, passed=True, expected_exit=1, cleanup=report['cleanup']))
        (out / 'checks.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
    print(json.dumps(dict(status='passed', checks=len(results), connections=900)), flush=True)


if __name__ == '__main__':
    main()
