#!/usr/bin/env python3
"""List architecture decisions from their files; never write a catalog or allocate identities."""
import argparse
from contextlib import contextmanager
import os
from pathlib import Path
import re
import stat

ROOT = Path(__file__).resolve().parent.parent


@contextmanager
def open_directory(directory):
    """Pin every directory component without following directory symlinks."""
    absolute = Path(directory).absolute()
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    descriptor = os.open(absolute.anchor, flags)
    try:
        for component in absolute.parts[1:]:
            child = os.open(component, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        yield descriptor
    finally:
        os.close(descriptor)


def read_records(descriptor):
    """Read only regular Markdown files relative to the pinned directory."""
    with os.scandir(descriptor) as entries:
        names = sorted(entry.name for entry in entries if entry.name.endswith('.md'))
    for name in names:
        mode = os.stat(name, dir_fd=descriptor, follow_symlinks=False).st_mode
        if stat.S_ISLNK(mode):
            raise ValueError('symlink entry refused: ' + name)
        if name == 'README.md':
            continue
        if not stat.S_ISREG(mode):
            raise ValueError('not a regular file: ' + name)
        # NOFOLLOW closes the stat/open race; NONBLOCK prevents a raced-in FIFO from hanging.
        child = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=descriptor)
        with os.fdopen(child, encoding='utf-8') as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise ValueError('not a regular file: ' + name)
            yield name, stream.read()


def catalog(directory):
    """Preserve legacy filenames/status wording without imposing a numbering policy."""
    rows = []
    with open_directory(directory) as descriptor:
        for name, text in read_records(descriptor):
            title = re.search(r'^#\s+(.+)$', text, re.MULTILINE)
            # Existing records use plain or bold Status, sometimes after Date on the same line.
            metadata = next((paragraph for paragraph in text.split('\n\n')
                             if re.search(r'\bStatus\s*:', paragraph, re.IGNORECASE)), '')
            status = re.search(r'\bStatus\s*:\s*(.*)', metadata.replace('**', ''),
                               re.IGNORECASE | re.DOTALL)
            rows.append((name, title.group(1).strip() if title else Path(name).stem,
                         ' '.join(status.group(1).split()) if status else 'unspecified'))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['list'])
    parser.add_argument('--directory', type=Path, default=ROOT / 'docs/adr',
                        help='ADR directory (default: this checkout\'s docs/adr)')
    args = parser.parse_args()
    try:
        rows = catalog(args.directory)
    except FileNotFoundError:
        parser.exit(1, 'adr: directory does not exist or an entry disappeared: ' +
                    str(args.directory) + '\n')
    except (OSError, UnicodeError, ValueError) as error:
        parser.exit(1, 'adr: ' + str(error) + '\n')
    for name, title, status in rows:
        print(f'- [{title}]({name}) — {status}')


if __name__ == '__main__':
    main()
