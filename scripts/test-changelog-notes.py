#!/usr/bin/env python3
"""Exercise scripts/changelog-notes.py's bullets-only rule for changelog.d/ fragments in a
temporary repository; never this checkout's own CHANGELOG.md or notes."""
from pathlib import Path
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from urllib.parse import urljoin, urlsplit

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

    def test_a_backtick_in_the_info_string_opens_no_fence(self):
        # "```lang`" is inline code, not a fence: the heading after it is outside any fence.
        self.note('info.md', BULLETS + '\n```lang`\n# Title\n')
        self.assertRefused(self.notes('--check'), 'info.md:5', '# Title')
        changelog = (self.repo / 'CHANGELOG.md').read_bytes()
        fragment = (self.repo / 'changelog.d/info.md').read_bytes()
        self.assertRefused(self.notes('--write', '## [9.9.9] - 2026-10-29'), 'info.md:5',
                           '# Title')
        self.assertEqual((self.repo / 'CHANGELOG.md').read_bytes(), changelog)
        self.assertEqual((self.repo / 'changelog.d/info.md').read_bytes(), fragment)

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

    def test_rc5_retains_all_five_merged_adapter_notes_with_resolving_adr_link(self):
        self.assert_rc5_notes(SCRIPTS.parent)

    def test_historical_rc4_yank_explains_tagged_but_unpublished_qemu_failure(self):
        changelog = (SCRIPTS.parent / 'CHANGELOG.md').read_text()
        rc4 = changelog.split('## [0.14.0-rc.4] - 2026-11-02 [YANKED]\n', 1)[1].split(
            '\n## [', 1)[0]
        explanation = rc4.split('### Authority compatibility', 1)[0]
        self.assertIn('Tagged but no release assets were published', explanation)
        self.assertIn('Ubuntu 26.04 no longer supplied the unambiguous', explanation)
        self.assertIn('`qemu-user-static` package required by the Linux ARM64 build.', explanation)

    def test_historical_rc5_notes_allow_later_version_and_new_adapter_fragment(self):
        (self.repo / 'CHANGELOG.md').write_bytes((SCRIPTS.parent / 'CHANGELOG.md').read_bytes())
        (self.repo / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.15.0"\n')
        self.note('scenario-github-future.md', '- A later GitHub Scenario change.\n')
        self.assert_rc5_notes(self.repo)
        result = self.notes('--check')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def assert_rc5_notes(self, root):
        changelog = (root / 'CHANGELOG.md').read_bytes()
        rc5 = changelog.split(b'## [0.14.0-rc.5] - 2026-11-02\n', 1)[1].split(
            b'\n## [0.14.0-rc.4]', 1)[0]
        notes = (
            (
                b'- Add an optional installed GitHub Scenario adapter with exact locked-head publication,\n'
                b'  durable reconciliation, explicit CI identities, owner review requests and approved follow-up\n'
                b'  issues. Configuration and credentials remain machine-owned; uncertain effects never authorize\n'
                b'  duplicate requests. Includes inert examples and mock-backed fast-gate coverage; no live GitHub\n'
                b'  validation or active instances ([ADR-0275]('
            ),
            (
                b"- Forward the isolated subprocess's six HOME/XDG paths to every af command in the full GitHub\n"
                b'  Scenario environment regression, checking the effective command environment after launcher\n'
                b'  defaults. Retain aliased TMPDIR, permissive umask and unchanged read-only source assertions.\n'
            ),
            (
                b'- Verify the full installed GitHub Scenario sequence with read-only source fixtures in an\n'
                b'  isolated subprocess using an aliased TMPDIR and permissive umask, including uncertain-effect\n'
                b'  recovery, exact-head checks, owner review, merge observation and follow-up publication.\n'
            ),
            (
                b'- Repair GitHub Scenario CLI fixtures under macOS temporary-path aliases and permissive umasks:\n'
                b'  canonicalize authority paths and create private fixture directories. Installer path and\n'
                b'  ownership checks remain enforced, with regression coverage for unsafe ancestors and aliases.\n'
            ),
            (
                b'- Initialize GitHub Scenario CLI test state as a private writable file even when the source\n'
                b"  fixtures are read-only. The installed production adapter's full PR, recovery, review and\n"
                b'  follow-up sequence now exercises read-only fixture copies and verifies they remain unchanged.\n'
            ),
        )
        self.assertEqual(len(notes), 5)
        for note in notes:
            self.assertEqual(rc5.count(note), 1)
        adapter_link = re.search(rb'\[ADR-0275\]\(([^)]+)\)\)\.', rc5)
        self.assertIsNotNone(adapter_link)
        self.assertEqual(self.assert_rc5_link_resolves(adapter_link[1].decode()),
                         'docs/adr/0275-install-a-durable-github-scenario-adapter.md')
        # Audit every rendered RC5 link, including any other inherited relative docs path.
        for link in re.findall(rb'\[[^\]]+\]\(([^)]+)\)', rc5):
            self.assert_rc5_link_resolves(link.decode())
        self.assertIn(b'fast releases require no fresh Pi measurement', rc5)
        self.assertIn(b'Pin both Linux release builds to Ubuntu 24.04', rc5)

    def assert_rc5_link_resolves(self, link):
        base = 'https://github.com/PakhomovAlexander/afactory/blob/main/'
        resolved = urlsplit(urljoin(base + 'CHANGELOG.md', link))
        prefix = '/PakhomovAlexander/afactory/blob/main/'
        self.assertEqual(resolved.netloc, 'github.com')
        self.assertTrue(resolved.path.startswith(prefix), (link, resolved.path))
        target = resolved.path.removeprefix(prefix)
        self.assertTrue((SCRIPTS.parent / target).is_file(), target)
        return target

    def test_pi_documentation_links_resolve_in_fragments_and_rendered_root_notes(self):
        root = SCRIPTS.parent
        base = 'https://github.com/PakhomovAlexander/afactory/blob/main/'
        notes = sorted((root / 'changelog.d').glob('pi-*.md'))
        # Release consumes the real fragments. Keep exercising both documented targets
        # afterwards as well; while notes exist, render and audit every shipped Pi fragment.
        contents = {Path('pi-doc-links-regression.md'): (
            '- See [Pi architecture](' + base +
            'docs/adr/2026-11-02-own-the-pi-sdk-host-and-confine-its-tools.md) and\n'
            '  [Pi validation](' + base + 'docs/design/pi-provider-validation.md).\n')}
        contents.update({path: path.read_text() for path in notes})
        for path, content in contents.items():
            self.note(path.name, content)
        result = self.notes('--write', '## [9.9.9] - today')
        self.assertEqual(result.returncode, 0, result.stderr)
        rendered = (self.repo / 'CHANGELOG.md').read_text()
        documents = []
        for path, content in contents.items():
            self.assertIn(content.strip(), rendered)
            links = re.findall(r'\[[^\]]+\]\(([^)]+)\)', content)
            for link in links:
                if 'docs/' not in link:
                    continue
                # The renderer copies fragments unchanged: resolve the same target at both
                # GitHub browsing locations, without network access or touching real notes.
                for context in ['changelog.d/' + path.name, 'CHANGELOG.md']:
                    resolved = urlsplit(urljoin(base + context, link))
                    self.assertEqual(resolved.netloc, 'github.com')
                    prefix = '/PakhomovAlexander/afactory/blob/main/docs/'
                    self.assertTrue(resolved.path.startswith(prefix), (context, link, resolved.path))
                    target = resolved.path.removeprefix('/PakhomovAlexander/afactory/blob/main/')
                    self.assertTrue((root / target).is_file(), target)
                    documents.append(link)
        self.assertGreaterEqual(len(documents), 4, 'cover ADR and validation links in both contexts')
        for link in documents:
            self.assertIn('](' + link + ')', rendered)
        self.assertEqual(list((self.repo / 'changelog.d').glob('pi-*.md')), [])


if __name__ == '__main__':
    unittest.main()
