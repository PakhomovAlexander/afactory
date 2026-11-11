"""Admission of content-addressed diagnostic XZ symbols (ADR-0224).

Compressed integrity and decompressed identity are independent checks. Shared captures
validate one archive once. No decompressed executable is written or kept in memory.
"""
import hashlib
import lzma
import os
import time

from . import admission, limits
from .budget import Budget
from .values import ContractError, require

CHUNK = 64 * 1024
OUTPUT_CHUNK = 1 << 20
MEMORY_BYTES = 64 * 1024 * 1024
# All distinct originals together fit the existing build-artifact bound; the compressed
# members fit the existing diagnostic bundle bound. One deadline covers all archives.
DECOMPRESSED_BYTES = limits.BUILD_ARTIFACT_BYTES_MAX
SECONDS = limits.DIAGNOSTIC_TOOL_SECONDS_MAX


def admit(root, doc, hashes, budget=None):
    """Admit every retained archive from an already manifest-verified profiles bundle.

    The contract has checked state/path/shared-reference consistency. Reopen each member
    through confined descriptors and hash its compressed bytes again while decoding, so
    replacement since manifest verification cannot change the admitted evidence.
    """
    timing = Budget(time.monotonic() + SECONDS, time.monotonic, 0)
    archives = {}
    for capture in doc["captures"]:
        archive = capture["symbols"]["archive"]
        if archive is not None:
            archives[archive["path"]] = (archive, capture["binary_sha256"])
    require({path for path in hashes if path.startswith("symbols/")} == set(archives),
            "symbols: bundle holds unrelated or missing symbol archives")
    total = 0
    for path, (artifact, binary) in sorted(archives.items()):
        require(hashes.get(path) == artifact["sha256"],
                "symbols: archive is not the verified member its profiles name")
        require(0 < artifact["bytes"] <= min(limits.DIAGNOSTIC_BYTES_MAX,
                                              doc["budgets"]["bytes"]["limit"]),
                "symbols: archive exceeds the compressed byte bound")
        timing.seconds("admitting symbols")
        admission.check(budget, "admitting symbols")
        fd, info = admission.open_confined(root, path, "symbols")
        try:
            require(info.st_size == artifact["bytes"], "symbols: archive size differs")
            decoder = lzma.LZMADecompressor(format=lzma.FORMAT_XZ, memlimit=MEMORY_BYTES)
            compressed, original = hashlib.sha256(), hashlib.sha256()
            size = 0
            while True:
                timing.seconds("admitting symbols")
                admission.check(budget, "admitting symbols")
                chunk = os.read(fd, min(CHUNK, artifact["bytes"] - size + 1))
                if not chunk:
                    break
                size += len(chunk)
                require(size <= artifact["bytes"], "symbols: archive grew")
                compressed.update(chunk)
                require(not decoder.eof, "symbols: trailing or concatenated XZ data")
                while True:
                    timing.seconds("decompressing symbols")
                    admission.check(budget, "decompressing symbols")
                    output = decoder.decompress(chunk, max_length=min(
                        OUTPUT_CHUNK, DECOMPRESSED_BYTES - total + 1))
                    chunk = b""
                    total += len(output)
                    require(total <= DECOMPRESSED_BYTES,
                            "symbols: exceeds the decompressed byte bound")
                    original.update(output)
                    if decoder.eof or decoder.needs_input:
                        break
                require(not decoder.unused_data, "symbols: trailing or concatenated XZ data")
            require(size == artifact["bytes"], "symbols: archive shrank")
            require("sha256:" + compressed.hexdigest() == artifact["sha256"],
                    "symbols: compressed archive hash differs")
            require(decoder.eof, "symbols: truncated XZ archive")
            require("sha256:" + original.hexdigest() == binary,
                    "symbols: decompressed identity differs from binary_sha256")
            timing.seconds("admitting symbols")
            admission.check(budget, "admitting symbols")
        except lzma.LZMAError as error:
            raise ContractError("symbols: invalid XZ archive: %s" % error) from error
        finally:
            os.close(fd)
