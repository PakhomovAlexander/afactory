#!/usr/bin/env python3
"""List architecture decisions from their files; never write a catalog or allocate identities."""
import argparse
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent


def catalog(directory):
    """Preserve legacy filenames/status wording without imposing a numbering policy."""
    rows = []
    for path in sorted(directory.glob('*.md')):
        if path.name == 'README.md' or not path.is_file():
            continue
        text = path.read_text(encoding='utf-8')
        title = re.search(r'^#\s+(.+)$', text, re.MULTILINE)
        # Existing records use plain or bold Status, sometimes after Date on the same line.
        metadata = next((paragraph for paragraph in text.split('\n\n')
                         if re.search(r'\bStatus\s*:', paragraph, re.IGNORECASE)), '')
        status = re.search(r'\bStatus\s*:\s*(.*)', metadata.replace('**', ''),
                           re.IGNORECASE | re.DOTALL)
        rows.append((path.name, title.group(1).strip() if title else path.stem,
                     ' '.join(status.group(1).split()) if status else 'unspecified'))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['list'])
    parser.add_argument('--directory', type=Path, default=ROOT / 'docs/adr',
                        help='ADR directory (default: this checkout\'s docs/adr)')
    args = parser.parse_args()
    if not args.directory.is_dir():
        parser.exit(1, 'adr: directory does not exist: ' + str(args.directory) + '\n')
    try:
        rows = catalog(args.directory)
    except (OSError, UnicodeError) as error:
        parser.exit(1, 'adr: ' + str(error) + '\n')
    for name, title, status in rows:
        print(f'- [{title}]({name}) — {status}')


if __name__ == '__main__':
    main()
