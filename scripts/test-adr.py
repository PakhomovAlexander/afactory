#!/usr/bin/env python3
"""Exercise file-derived ADR discovery and independent additions in private repositories."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts/adr.py'


class CatalogTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.directory = self.root / 'docs/adr'
        self.directory.mkdir(parents=True)
        self.env = {**os.environ, 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
                    'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                    'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}

    def cli(self, *args, script=SCRIPT):
        return subprocess.run([sys.executable, '-I', '-B', str(script), 'list', *args],
                              cwd=self.root, capture_output=True, text=True, timeout=30)

    def git(self, *args):
        return subprocess.run([shutil.which('git'), *args], cwd=self.root, env=self.env,
                              capture_output=True, text=True, check=True, timeout=30).stdout

    def test_legacy_exceptions_and_date_topics_have_deterministic_titles_statuses_and_links(self):
        fixtures = {
            '2026-11-02-another-topic.md': '# Another topic\n\nDate: today. Status: proposed.\n',
            '0107-release.md': '# ADR-0107: Release\n\n**Status:** accepted (2026-09-01).\n',
            '0107-warm.md': '# ADR-0107: Warm\n\nStatus: Accepted; superseded in part by\n'
                            '[a successor](2026-11-02-another-topic.md).\n',
            '0240-a.md': '# Browser\n\nStatus: accepted\n',
            '0240-b.md': '# Executor\n\nStatus: Superseded by a successor\n',
            '2026-11-02-specific-topic.md': '# Specific topic\n\n**Status:** rejected (today).\n'}
        (self.directory / 'README.md').write_text('# Guide\n\nStatus: not a decision\n')
        (self.directory / 'scratch.txt').write_text('not a decision')
        for name, text in fixtures.items():
            (self.directory / name).write_text(text)
        before = {path.name: path.read_bytes() for path in self.directory.iterdir()}
        result = self.cli('--directory', str(self.directory))
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertEqual(len(lines), len(fixtures))
        for line, name in zip(lines, sorted(fixtures)):
            self.assertIn(f']({name}) — ', line)
            self.assertIn(fixtures[name].splitlines()[0][2:], line)
        self.assertIn('Accepted; superseded in part by [a successor]', result.stdout)
        self.assertIn('proposed.', result.stdout)
        self.assertIn('rejected (today).', result.stdout)
        self.assertNotIn('README.md', result.stdout)
        self.assertEqual(self.cli('--directory', str(self.directory)).stdout, result.stdout)
        self.assertEqual(before, {path.name: path.read_bytes() for path in self.directory.iterdir()})

    def test_default_directory_uses_script_checkout_and_handles_undated_legacy_metadata(self):
        (self.root / 'scripts').mkdir()
        copied = self.root / 'scripts/adr.py'
        shutil.copyfile(SCRIPT, copied)
        (self.directory / 'old-record.md').write_text('historical record without metadata\n')
        result = self.cli(script=copied)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '- [old-record](old-record.md) — unspecified\n')

    def test_empty_directory_and_missing_directory_are_distinct(self):
        result = self.cli('--directory', str(self.directory))
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
        missing = self.cli('--directory', str(self.directory / 'missing'))
        self.assertEqual(missing.returncode, 1)
        self.assertEqual(missing.stdout, '')
        self.assertIn('directory does not exist', missing.stderr)

    def test_independent_same_date_additions_merge_without_shared_edits_and_are_discovered(self):
        guide = (ROOT / 'docs/adr/README.md').read_bytes()
        (self.directory / 'README.md').write_bytes(guide)
        self.git('init', '-q', '-b', 'main')
        self.git('add', '.')
        self.git('commit', '-qm', 'stable guide')
        names = ['2026-11-02-confine-file-tools.md', '2026-11-02-pin-scenario-actions.md']
        for branch, name in zip(['tools', 'scenarios'], names):
            self.git('checkout', '-qb', branch, 'main')
            (self.directory / name).write_text(f'# ADR-{name[:-3]}: {branch}\n\n'
                                               '**Status:** proposed (2026-11-02).\n')
            self.git('add', str(Path('docs/adr') / name))
            self.git('commit', '-qm', branch)
            self.assertEqual(self.git('diff', '--name-only', 'main', 'HEAD').splitlines(),
                             ['docs/adr/' + name])
        self.git('merge', '--no-edit', 'tools')
        self.assertEqual((self.directory / 'README.md').read_bytes(), guide)
        self.assertEqual(self.git('status', '--porcelain'), '')
        result = self.cli('--directory', str(self.directory))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), 2)
        for name in names:
            self.assertIn(f']({name})', result.stdout)
        self.assertEqual(set(self.git('diff', '--name-only', 'main', 'HEAD').splitlines()),
                         {'docs/adr/' + name for name in names})

    def test_checkout_catalog_discovers_every_decision_without_a_central_manifest(self):
        result = self.cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        files = sorted(path.name for path in (ROOT / 'docs/adr').glob('*.md')
                       if path.name != 'README.md')
        self.assertEqual(len(result.stdout.splitlines()), len(files))
        for line, name in zip(result.stdout.splitlines(), files):
            self.assertIn(f']({name}) — ', line)


if __name__ == '__main__':
    unittest.main()
