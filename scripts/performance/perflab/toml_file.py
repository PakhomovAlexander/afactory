"""Read TOML through the vendored, unmodified Tomli 2.2.1 on every supported Python.

Floats and dates are refused: every number the laboratory reads from TOML is an integer or
a canonical decimal string. On Python 3.11+ the parity test compares this parser with the
standard library's `tomllib` over the committed registries.
"""
from .values import ContractError, require
from ._vendor import tomli

MAX_TOML_BYTES = 1024 * 1024


def _refuse_float(text):
    raise ContractError("TOML floats are not allowed; write the decimal as a string: " + text)


def _check_plain(value, where):
    if isinstance(value, dict):
        for key, item in value.items():
            _check_plain(item, where + "." + key)
    elif isinstance(value, list):
        for index, item in enumerate(value):
            _check_plain(item, "%s[%d]" % (where, index))
    else:
        require(isinstance(value, (str, bool, int)),
                where + ": TOML dates and times are not allowed")


def loads(data, where, parser=None):
    require(isinstance(data, (bytes, bytearray)), where + ": expected bytes")
    require(len(data) <= MAX_TOML_BYTES, where + ": exceeds %d bytes" % MAX_TOML_BYTES)
    try:
        text = bytes(data).decode("utf-8")
    except UnicodeDecodeError as error:
        raise ContractError(where + ": not UTF-8: " + str(error)) from error
    module = parser or tomli
    try:
        value = module.loads(text, parse_float=_refuse_float)
    except ContractError as error:
        raise ContractError(where + ": " + str(error)) from error
    except module.TOMLDecodeError as error:
        raise ContractError(where + ": invalid TOML: " + str(error)) from error
    _check_plain(value, where)
    return value
