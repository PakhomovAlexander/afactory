"""Build image identities of the laboratory (ADR-0170).

A Linux recipe builds in an image named by digest in one of two canonical forms:

- `sha256:<64 hex>`, a local image ID: the image `docker image inspect` reports with exactly
  that ID;
- `name@sha256:<64 hex>`, a repository manifest digest: an image whose `RepoDigests` holds
  exactly that reference. Its local image ID is whatever the image has; a manifest digest is
  never assumed to equal an image ID, and a digest under another repository name is not it.

A name is lowercase repository components separated by `/`, the first optionally a registry
host with a port, and carries no tag. The plan freezes the reference; the build records the
reference it ran and the image ID it resolved; the comparator requires the recorded reference
to be exactly the frozen one, and for an ID pin also the recorded ID.
"""
import re

from .spec import Str
from .values import require

_COMPONENT = r"[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*"
_NAME = r"(?:%s(?::[0-9]{1,5})?/)*%s" % (_COMPONENT, _COMPONENT)
_DIGEST = r"sha256:[0-9a-f]{64}"
REFERENCE_PATTERN = r"^(?:%s@)?%s$" % (_NAME, _DIGEST)
REFERENCE = Str(pattern=REFERENCE_PATTERN, max_len=330)
_REFERENCE = re.compile(REFERENCE_PATTERN)


def canonical(reference, where="build image"):
    """`reference` when it names an image by digest in a canonical form, else refused."""
    require(isinstance(reference, str) and len(reference) <= 330
            and _REFERENCE.match(reference) is not None,
            "%s: %r is not an image pinned by digest in canonical form (sha256:<image ID> or "
            "lowercase name@sha256:<manifest digest>, without a tag)" % (where, reference))
    return reference


def is_image_id(reference):
    return reference.startswith("sha256:")


def mismatch(frozen, recorded):
    """Why the recorded build image `{reference, id}` is not the frozen reference, or None."""
    if recorded is None:
        return "the build ran in no image"
    if recorded["reference"] != frozen:
        return "the build ran %s, not the frozen %s" % (recorded["reference"], frozen)
    if is_image_id(frozen) and recorded["id"] != frozen:
        return "the build's image ID is %s, not the frozen %s" % (recorded["id"], frozen)
    return None
