"""Shared value rules: identifiers, digests, exact decimals and rationals, canonical JSON, paths.

Every reader and writer of the laboratory validates through these helpers, so the same
spelling rule never exists twice.
"""
from fractions import Fraction
import hashlib
import json
import re


class ContractError(ValueError):
    """A document, value or input violates its closed contract."""


def require(condition, message):
    if not condition:
        raise ContractError(message)


def add_note(error, note):
    """`error.add_note(note)`, also on Python 3.9 and 3.10, which lack it (ADR-0162)."""
    if hasattr(error, "add_note"):
        error.add_note(note)
    else:
        error.__notes__ = list(getattr(error, "__notes__", ())) + [note]


ID_PATTERN = r"^[a-z][a-z0-9_]{0,63}$"
GIT_SHA_PATTERN = r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"
DIGEST_PATTERN = r"^sha256:[0-9a-f]{64}$"
# Canonical nonnegative decimal: no sign, exponent, leading or trailing redundant zero.
DECIMAL_PATTERN = r"^(?:0|[1-9][0-9]*)(?:\.[0-9]*[1-9])?$"
# Exact signed rational in lowest terms; an integer is written without a denominator.
RATIONAL_PATTERN = r"^-?(?:0|[1-9][0-9]*)(?:/[1-9][0-9]*)?$"
PATH_SEGMENT_PATTERN = r"^[A-Za-z0-9._-]{1,128}$"
UNITS = ("ms", "bytes", "count", "ratio")

MAX_DECIMAL_DIGITS = 38
MAX_RATIONAL_CHARS = 512
MAX_PATH_CHARS = 1024
RECORD_MAX_BYTES = 64 * 1024

_ID = re.compile(ID_PATTERN)
_SHA = re.compile(GIT_SHA_PATTERN)
_DIGEST = re.compile(DIGEST_PATTERN)
_DECIMAL = re.compile(DECIMAL_PATTERN)
_RATIONAL = re.compile(RATIONAL_PATTERN)
_SEGMENT = re.compile(PATH_SEGMENT_PATTERN)


def is_id(value):
    return isinstance(value, str) and _ID.match(value) is not None


def check_id(value, where):
    require(is_id(value), where + ": expected an ID matching [a-z][a-z0-9_]{0,63}")
    return value


def check_git_sha(value, where):
    require(isinstance(value, str) and _SHA.match(value) is not None,
            where + ": expected a full lowercase Git object ID")
    return value


def check_digest(value, where):
    require(isinstance(value, str) and _DIGEST.match(value) is not None,
            where + ": expected sha256:<64 lowercase hex>")
    return value


def sha256_bytes(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


# Exact decimals ---------------------------------------------------------------------------

def significant_digits(text):
    digits = text.replace(".", "").lstrip("0")
    return max(1, len(digits))


def check_decimal(value, where):
    """A canonical nonnegative decimal string with at most 38 significant digits."""
    require(isinstance(value, str) and _DECIMAL.match(value) is not None,
            where + ": expected a canonical nonnegative decimal string (no exponent, sign or "
            "redundant zero)")
    require(significant_digits(value) <= MAX_DECIMAL_DIGITS,
            where + ": a decimal has at most 38 significant digits")
    return value


def parse_decimal(value, where):
    check_decimal(value, where)
    return Fraction(value)


def format_decimal(number, where="decimal"):
    """The canonical spelling of a nonnegative terminating Fraction (or int)."""
    number = Fraction(number)
    require(number >= 0, where + ": a metric decimal is nonnegative")
    den = number.denominator
    twos = fives = 0
    while den % 2 == 0:
        den //= 2
        twos += 1
    while den % 5 == 0:
        den //= 5
        fives += 1
    require(den == 1, where + ": value has no terminating decimal expansion")
    scale = max(twos, fives)
    scaled = number.numerator * (10 ** scale) // number.denominator
    text = str(scaled)
    if scale:
        text = text.rjust(scale + 1, "0")
        text = (text[:-scale] + "." + text[-scale:]).rstrip("0").rstrip(".")
    return check_decimal(text, where)


# Exact signed rationals --------------------------------------------------------------------

def check_rational(value, where):
    require(isinstance(value, str) and len(value) <= MAX_RATIONAL_CHARS
            and _RATIONAL.match(value) is not None,
            where + ": expected an exact rational such as -3/4")
    require(value != "-0", where + ": zero is written 0")
    if "/" in value:
        num, den = value.split("/")
        number = Fraction(int(num), int(den))
        require(int(den) > 1 and number.denominator == int(den),
                where + ": a rational is written in lowest terms with a denominator above 1")
    return value


def parse_rational(value, where):
    check_rational(value, where)
    return Fraction(value)


def format_rational(number):
    number = Fraction(number)
    if number.denominator == 1:
        return str(number.numerator)
    return "%d/%d" % (number.numerator, number.denominator)


# JSON --------------------------------------------------------------------------------------

def _refuse_float(text):
    raise ContractError("JSON numbers must be integers; found " + text)


def _refuse_constant(text):
    raise ContractError("JSON constant " + text + " is not allowed")


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def loads_strict(data, where, max_bytes):
    """Parse UTF-8 JSON bytes refusing duplicates, floats, NaN and oversize input."""
    require(isinstance(data, (bytes, bytearray)), where + ": expected bytes")
    require(len(data) <= max_bytes, where + ": exceeds %d bytes" % max_bytes)
    require(not data.startswith(b"\xef\xbb\xbf"), where + ": a byte-order mark is not allowed")
    try:
        text = bytes(data).decode("utf-8")
    except UnicodeDecodeError as error:
        raise ContractError(where + ": not UTF-8: " + str(error)) from error
    try:
        return json.loads(text, object_pairs_hook=_unique_object, parse_float=_refuse_float,
                          parse_constant=_refuse_constant)
    except ContractError as error:
        raise ContractError(where + ": " + str(error)) from error
    except ValueError as error:
        raise ContractError(where + ": invalid JSON: " + str(error)) from error


def _check_canonical_value(value, where):
    if isinstance(value, dict):
        for key, item in value.items():
            require(isinstance(key, str), where + ": object keys must be strings")
            _check_canonical_value(item, where + "." + key)
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _check_canonical_value(item, "%s[%d]" % (where, index))
    else:
        require(value is None or isinstance(value, (str, bool, int)),
                where + ": canonical JSON holds no floats or foreign values")


def canonical_bytes(value):
    """Sorted keys, UTF-8, compact separators, no floats; array order is significant."""
    _check_canonical_value(value, "$")
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"),
                      allow_nan=False).encode("utf-8")


def canonical_digest(value):
    return sha256_bytes(canonical_bytes(value))


def document_bytes(value):
    """The bytes a writer stores: canonical JSON and one terminating newline."""
    return canonical_bytes(value) + b"\n"


# Relative paths ----------------------------------------------------------------------------

def check_relative_path(value, where):
    """A relative POSIX path with no empty, '.', '..' or unusual segments."""
    require(isinstance(value, str) and 0 < len(value) <= MAX_PATH_CHARS,
            where + ": expected a nonempty relative path")
    require(not value.startswith("/") and "\\" not in value and "\x00" not in value,
            where + ": a path is relative and uses '/' separators")
    for segment in value.split("/"):
        require(_SEGMENT.match(segment) is not None and segment not in (".", ".."),
                where + ": path segment %r is not allowed" % segment)
    return value
