"""Select the repository's build/test rules for miniature gate fixtures."""
import re


def test_recipe(makefile):
    """Keep complete test rules, including Make conditionals, until the next target.

    This handles the repository's ordinary target syntax, not arbitrary Make syntax.
    Comments and target ordering are not fixture boundaries.
    """
    wanted = {'test-build', 'test'}
    seen = set()
    selected = []
    active = False
    for line in makefile.splitlines(keepends=True):
        target = re.match(r'^([A-Za-z0-9_.%/-]+)\s*:(?!=)', line)
        if target:
            name = target.group(1)
            active = name in wanted
            if active:
                if name in seen:
                    raise ValueError(f'duplicate fixture target: {name}')
                seen.add(name)
        if active:
            selected.append(line)
    if seen != wanted:
        raise ValueError(f'missing fixture targets: {sorted(wanted - seen)}')
    return ''.join(selected)
