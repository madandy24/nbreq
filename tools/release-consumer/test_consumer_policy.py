"""Focused offline checks for explicit policy selection and evidence preservation."""
from pathlib import Path
import tempfile
import unittest

from consumer_policy import select_consumer_lock


LOCK = '''version = 4
[[package]]
name = "yoke-derive"
version = "0.8.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "original"
'''


class PolicyTests(unittest.TestCase):
    def exercise(self, version, older):
        with tempfile.TemporaryDirectory() as directory:
            consumer = Path(directory)/'consumer'
            consumer.mkdir()
            lock = consumer/'Cargo.lock'
            lock.write_text(LOCK)
            evidence = Path(directory)/'evidence'
            calls = []

            def run(label, command, cwd):
                calls.append(command)
                self.assertEqual(cwd, consumer)
                if command[-1] == '-vV':
                    return ('rustc '+version+'\nrelease: '+version+'\n').encode()
                self.assertEqual(command, ['cargo', 'update', '--offline', '-p', 'yoke-derive', '--precise', '0.8.2'])
                lock.write_text(LOCK.replace('0.8.3', '0.8.2').replace('original', 'selected'))
                return b''

            result = select_consumer_lock(run, 'check', ['cargo'], 'arbitrary-alias', consumer,
                                          evidence, ['--offline'])
            self.assertEqual((evidence/'Cargo.initial.lock').read_text(), LOCK)
            self.assertEqual((evidence/'Cargo.lock').read_bytes(), lock.read_bytes())
            self.assertEqual(len(calls), 2 if older else 1)
            self.assertEqual(result['route'], 'compatibility-selected' if older else 'fresh')
            self.assertEqual(result['initial']['sha256'] == result['selected']['sha256'], not older)
            self.assertEqual(len(result['removed']), 1 if older else 0)

    def test_effective_compiler_boundary_not_toolchain_alias(self):
        for version, older in [('1.85.0', True), ('1.86.0', True), ('1.87.0', False), ('1.97.1', False)]:
            with self.subTest(version=version):
                self.exercise(version, older)

    def test_update_failure_is_not_retried_or_relabelled(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path/'Cargo.lock').write_text(LOCK)
            calls = []

            def run(label, command, cwd):
                calls.append(command)
                if command[-1] == '-vV':
                    return b'release: 1.85.0\n'
                raise RuntimeError('update failed')

            with self.assertRaisesRegex(RuntimeError, 'update failed'):
                select_consumer_lock(run, 'check', ['cargo'], 'stable', path, path/'evidence')
            self.assertEqual(len(calls), 2)
            self.assertEqual((path/'evidence/Cargo.initial.lock').read_text(), LOCK)
            self.assertIn('"status": "selecting"', (path/'evidence/consumer-policy.json').read_text())


if __name__ == '__main__':
    unittest.main()
