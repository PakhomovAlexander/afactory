#!/usr/bin/env python3
"""Exercise scripts/changelog-notes.py's bullets-only rule for changelog.d/ fragments in a
temporary repository; never this checkout's own CHANGELOG.md or notes."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
GIT = shutil.which('git')
POINTER = ('Changes since the last release are notes under [`changelog.d/`](changelog.d/), one file per\n'
           'pull request; the release pull request collects them here.')
CHANGELOG = f'# Changelog\n\n## [Unreleased]\n\n{POINTER}\n\n## [0.1.0] - today\n\n- change\n'
BULLETS = '- Fold the title into the first bullet.\n  Wrapped continuation.\n'
FIX = 'A fragment holds bullets; see changelog.d/README.md'


class Fragments(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        (self.repo / 'scripts').mkdir()
        (self.repo / 'changelog.d').mkdir()
        shutil.copy(SCRIPTS / 'changelog-notes.py', self.repo / 'scripts' / 'changelog-notes.py')
        (self.repo / 'CHANGELOG.md').write_text(CHANGELOG)
        # The README is the one file under changelog.d/ that may hold headings.
        (self.repo / 'changelog.d/README.md').write_text('# Change notes\n\n## Shape\n')
        self.env = {**os.environ, 'PYTHONDONTWRITEBYTECODE': '1',
                    'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1',
                    'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                    'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}
        self.git('init', '-q', '-b', 'main')

    def git(self, *args):
        return subprocess.run([GIT, *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def note(self, name, text):
        (self.repo / 'changelog.d' / name).write_text(text)
        self.git('add', '.')
        self.git('commit', '-qm', name)

    def notes(self, *args):
        return subprocess.run(['python3', '-I', 'scripts/changelog-notes.py', *args], cwd=self.repo,
                              env=self.env, capture_output=True, text=True)

    def assertRefused(self, result, place, line):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'changelog.d/{place}: a heading in a change note: {line!r}', result.stderr)
        self.assertIn(FIX, result.stderr)

    def test_check_passes_a_bullet_only_fragment(self):
        self.note('bullets.md', BULLETS)
        result = self.notes('--check')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, '')

    def test_check_refuses_a_top_level_heading(self):
        self.note('top.md', '# Performance repairs\n\n' + BULLETS)
        self.assertRefused(self.notes('--check'), 'top.md:1', '# Performance repairs')

    def test_check_refuses_a_deeper_and_a_bare_heading_naming_each_line(self):
        self.note('deeper.md', BULLETS + '\n### Also\n\n' + BULLETS + '#\n')
        result = self.notes('--check')
        self.assertRefused(result, 'deeper.md:4', '### Also')
        self.assertRefused(result, 'deeper.md:8', '#')

    def test_check_ignores_hash_lines_in_a_fenced_code_block_and_non_headings(self):
        # A shorter fence, a fence followed by text or the other fence character does not close.
        self.note('fenced.md', BULLETS + '\n  ````sh\n  # a shell comment\n  ```\n  # inside\n'
                  '  ```` text\n  ~~~~\n# inside\n  ````\n~~~\n# tilde fenced\n~~~\n'
                  '- #123 and `#[ignore]` are not headings\n#hashtag\n')
        result = self.notes('--check')
        self.assertEqual(result.returncode, 0, result.stderr)
        # A heading after the fence closes is still refused.
        self.note('fenced.md', '```\n# inside\n```\n# outside\n')
        self.assertRefused(self.notes('--check'), 'fenced.md:4', '# outside')

    def test_an_overindented_pseudo_fence_hides_no_heading(self):
        # Four spaces make indented code, not a fence: the heading after it is outside any fence.
        self.note('indented.md', BULLETS + '\n    ```\n# Title\n')
        self.assertRefused(self.notes('--check'), 'indented.md:5', '# Title')
        changelog = (self.repo / 'CHANGELOG.md').read_bytes()
        fragment = (self.repo / 'changelog.d/indented.md').read_bytes()
        self.assertRefused(self.notes('--write', '## [9.9.9] - 2026-10-29'), 'indented.md:5',
                           '# Title')
        self.assertEqual((self.repo / 'CHANGELOG.md').read_bytes(), changelog)
        self.assertEqual((self.repo / 'changelog.d/indented.md').read_bytes(), fragment)

    def test_check_still_refuses_an_entry_under_unreleased(self):
        (self.repo / 'CHANGELOG.md').write_text(CHANGELOG.replace(POINTER, POINTER + '\n\n- entry'))
        result = self.notes('--check')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('[Unreleased] must hold only the pointer', result.stderr)

    def test_write_refuses_a_heading_fragment_and_changes_no_file(self):
        self.note('good.md', BULLETS)
        self.note('heading.md', '## Title\n\n' + BULLETS)
        before = {p: p.read_bytes() for p in (self.repo / 'CHANGELOG.md',
                                              *(self.repo / 'changelog.d').iterdir())}
        result = self.notes('--write', '## [0.2.0] - today')
        self.assertRefused(result, 'heading.md:1', '## Title')
        self.assertEqual({p: p.read_bytes() for p in before}, before)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_write_collects_bullet_only_fragments(self):
        self.note('good.md', BULLETS)
        result = self.notes('--write', '## [0.2.0] - today')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.repo / 'CHANGELOG.md').read_text(), CHANGELOG.replace(
            '## [0.1.0]', '## [0.2.0] - today\n\n' + BULLETS + '\n## [0.1.0]'))
        self.assertFalse((self.repo / 'changelog.d/good.md').exists())
        self.assertEqual(self.notes('--check').returncode, 0)


if __name__ == '__main__':
    unittest.main()
