"""Allowlist sanitization of a flamegraph SVG before a report embeds or links it (ADR-0174,
ADR-0175).

inferno's `--no-javascript` still leaves a `<script>` element and an `onload` handler, so its SVG
is never treated as trusted. The original bytes are kept and hashed separately; the sanitized
document is rebuilt from a namespace-aware parse:

- **Refused outright:** a DTD internal subset, any entity declaration, any reference to an
  entity that is not XML's own, external entities, a root other than one `<svg>` in the SVG
  namespace, non-UTF-8 input, input beyond its byte bound and documents beyond the element or
  depth bound.
- **Dropped:** comments, processing instructions and a DOCTYPE without an internal subset (its
  external DTD is never fetched), and every namespace declaration: the output declares only the
  SVG namespace and, when a kept attribute needs it, inferno's own `fg` namespace, on its root.
- **Kept:** only the SVG-namespace elements and the unqualified or `fg:` attributes inferno's
  flamegraphs are drawn with, each value matching its attribute's own grammar (numbers, colors,
  identifiers, a local `#id` paint reference for `fill`). An element of any other namespace
  (XHTML, MathML, a prefixed SVG alias that is not drawn) and anything else (`script`,
  `foreignObject`, `image`, `use`, animation, links) is removed with its content; an SVG `a`
  keeps its content as a plain group.
- **CSS:** a `<style>` block is kept only when all of it parses as the safe subset inferno writes:
  rules of plain selectors and declarations of an allowlisted property whose value is keywords,
  numbers, lengths, hex colors, `rgb()` or a quoted plain font name. Any other function
  (`url()`, `image-set()`, `expression()`), at-rule, escape, comment or markup removes the whole
  block; it is never repaired.

Every removal is counted, and the output is deterministic for the same input.
"""
import re
import xml.parsers.expat

from .values import ContractError

MAX_INPUT_BYTES = 64 * 1024 * 1024
MAX_ELEMENTS = 2000000
MAX_DEPTH = 256

ELEMENTS = frozenset(("svg", "g", "rect", "text", "title", "defs", "linearGradient", "stop",
                      "style"))
# Removed, but their children kept as a group.
UNWRAPPED = frozenset(("a",))
SVG_NAMESPACE = "http://www.w3.org/2000/svg"
INFERNO_NAMESPACE = "http://github.com/jonhoo/inferno"
# Kept for compatibility with existing callers.
XML_NAMESPACE = SVG_NAMESPACE
_NUMBER = r"-?[0-9]{1,9}(?:\.[0-9]{1,6})?"
_COLOR = (r"#[0-9A-Fa-f]{3,8}|[A-Za-z]{1,32}"
          r"|rgb\(\s*[0-9]{1,3}\s*,\s*[0-9]{1,3}\s*,\s*[0-9]{1,3}\s*\)")
_LENGTH = re.compile(r"^%s%%?$" % _NUMBER)
_VALUES = {
    "version": re.compile(r"^[0-9]{1,2}\.[0-9]{1,2}$"),
    "viewBox": re.compile(r"^%s(?: %s){3}$" % (_NUMBER, _NUMBER)),
    "fill": re.compile(r"^(?:%s|url\(#[A-Za-z0-9_-]{1,64}\))$" % _COLOR),
    "stop-color": re.compile(r"^(?:%s)$" % _COLOR),
    "id": re.compile(r"^[A-Za-z][A-Za-z0-9_-]{0,63}$"),
    "class": re.compile(r"^[A-Za-z][A-Za-z0-9_-]{0,63}(?: [A-Za-z][A-Za-z0-9_-]{0,63}){0,7}$"),
    "total_samples": re.compile(r"^[0-9]{1,20}$"),
    "type": re.compile(r"^text/css$"),
}
for _name in ("width", "height", "x", "y", "x1", "x2", "y1", "y2", "offset"):
    _VALUES[_name] = _LENGTH
# inferno's own attributes, in its `fg` namespace.
FG_VALUES = {"x": re.compile(r"^[0-9]{1,20}$"), "w": re.compile(r"^[0-9]{1,20}$")}
ATTRIBUTES = frozenset(_VALUES)
# The safe CSS subset: rules of plain selectors and allowlisted declarations.
CSS_PROPERTIES = frozenset(("font-family", "font-size", "font-weight", "font-color",
                            "text-anchor", "opacity", "cursor", "stroke", "stroke-width",
                            "display", "fill"))
_CSS_RULE = re.compile(r"\s*([^{}]+?)\s*\{([^{}]*)\}")
_CSS_SELECTOR = re.compile(r"^[A-Za-z0-9_#.*:> -]{1,128}$")
_CSS_TOKEN = (r"(?:%s|%s(?:px|em|%%)?|\"[A-Za-z0-9 _-]{1,64}\")" % (_COLOR, _NUMBER))
_CSS_VALUE = re.compile(r"^%s(?:\s*,?\s*%s){0,7}$" % (_CSS_TOKEN, _CSS_TOKEN))
_CSS_PROPERTY = re.compile(r"^[a-z-]{1,32}$")


class Sanitized(object):
    def __init__(self, data, removed_elements, removed_attributes):
        self.data = data
        self.removed_elements = removed_elements
        self.removed_attributes = removed_attributes


class _Node(object):
    __slots__ = ("name", "attributes", "children")

    def __init__(self, name, attributes):
        self.name = name
        self.attributes = attributes
        self.children = []


def _escape(text, quote=False):
    text = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    if quote:
        text = text.replace('"', "&quot;")
    return text


def _refuse(detail):
    raise ContractError("unsafe SVG refused: " + detail)


def sanitize(data):
    """The sanitized bytes of the SVG `data`, with what was removed; ContractError if refused."""
    if not isinstance(data, (bytes, bytearray)):
        _refuse("expected bytes")
    if len(data) > MAX_INPUT_BYTES:
        _refuse("%d bytes, over the %d-byte bound" % (len(data), MAX_INPUT_BYTES))
    try:
        bytes(data).decode("utf-8")
    except UnicodeDecodeError:
        _refuse("not UTF-8")
    # Names arrive as "NAMESPACE LOCAL" (or "LOCAL" without a namespace), so a prefix never
    # stands in for its namespace.
    parser = xml.parsers.expat.ParserCreate("UTF-8", " ")
    parser.ordered_attributes = True
    parser.buffer_text = True
    parser.SetParamEntityParsing(xml.parsers.expat.XML_PARAM_ENTITY_PARSING_NEVER)
    state = {"elements": 0, "removed_elements": 0, "removed_attributes": 0, "fg": False}
    root = _Node("#document", [])
    stack = [root]
    # Depth of an element being skipped with its whole subtree, or 0.
    skipping = [0]

    def doctype(name, system_id, public_id, has_internal_subset):
        if has_internal_subset:
            _refuse("a DTD internal subset")

    def entity(*_):
        _refuse("an entity declaration")

    def skipped(name, is_parameter):
        _refuse("a reference to the undeclared entity %r" % name)

    def external(*_):
        _refuse("an external entity")

    def namespace(prefix, uri):
        # Only the SVG default and inferno's `fg` are ever declared again, on the root.
        if (prefix, uri) not in ((None, SVG_NAMESPACE), ("fg", INFERNO_NAMESPACE)):
            state["removed_attributes"] += 1

    def start(name, attributes):
        state["elements"] += 1
        if state["elements"] > MAX_ELEMENTS:
            _refuse("more than %d elements" % MAX_ELEMENTS)
        if len(stack) + skipping[0] > MAX_DEPTH:
            _refuse("nesting deeper than %d" % MAX_DEPTH)
        if skipping[0]:
            skipping[0] += 1
            state["removed_elements"] += 1
            return
        uri, _, local = name.rpartition(" ")
        if len(stack) == 1:
            if root.children or local != "svg":
                _refuse("the root element is %r, not one <svg>" % local)
            if uri != SVG_NAMESPACE:
                _refuse("the root is not in the SVG namespace")
        if uri != SVG_NAMESPACE:
            state["removed_elements"] += 1
            skipping[0] = 1
            return
        if local in UNWRAPPED:
            state["removed_elements"] += 1
            local, attributes = "g", []
        elif local not in ELEMENTS:
            state["removed_elements"] += 1
            skipping[0] = 1
            return
        kept = []
        for index in range(0, len(attributes), 2):
            key, value = attributes[index], attributes[index + 1]
            shown = _attribute(local, key, value)
            if shown is None:
                state["removed_attributes"] += 1
            else:
                kept.append((shown, value))
                state["fg"] = state["fg"] or shown.startswith("fg:")
        node = _Node(local, kept)
        stack[-1].children.append(node)
        stack.append(node)

    def end(name):
        if skipping[0]:
            skipping[0] -= 1
            return
        stack.pop()

    def text(content):
        if skipping[0] or len(stack) == 1:
            return
        stack[-1].children.append(content)

    parser.StartDoctypeDeclHandler = doctype
    parser.EntityDeclHandler = entity
    parser.UnparsedEntityDeclHandler = entity
    parser.SkippedEntityHandler = skipped
    parser.ExternalEntityRefHandler = external
    parser.StartNamespaceDeclHandler = namespace
    parser.StartElementHandler = start
    parser.EndElementHandler = end
    parser.CharacterDataHandler = text
    # Comments and processing instructions have no handler: they are dropped.
    try:
        parser.Parse(bytes(data), True)
    except xml.parsers.expat.ExpatError as error:
        _refuse("not well-formed XML: %s" % error)
    if not root.children:
        _refuse("no <svg> element")
    top = root.children[0]
    declared = [("xmlns", SVG_NAMESPACE)]
    if state["fg"]:
        declared.append(("xmlns:fg", INFERNO_NAMESPACE))
    top.attributes = declared + top.attributes
    out = []
    _style_check(top, state)
    _write(top, out)
    return Sanitized(("".join(out) + "\n").encode("utf-8"), state["removed_elements"],
                     state["removed_attributes"])


def _attribute(element, key, value):
    """How a kept attribute is written, or None when it is removed: an unqualified attribute
    of the allowlist, or inferno's `fg:x`/`fg:w`, each with a value of its own grammar."""
    uri, _, local = key.rpartition(" ")
    if uri == INFERNO_NAMESPACE:
        grammar, shown = FG_VALUES.get(local), "fg:" + local
    elif uri:
        return None
    else:
        grammar, shown = _VALUES.get(local), local
    if grammar is None or grammar.match(value) is None:
        return None
    if local == "type" and element != "style":
        return None
    return shown


def safe_css(css):
    """Whether the whole style text is the safe subset: rules of plain selectors whose
    declarations are allowlisted properties with plain values (no function but rgb())."""
    position = 0
    text = css.strip()
    if not text:
        return True
    while position < len(text):
        match = _CSS_RULE.match(text, position)
        if match is None:
            return False
        for selector in match.group(1).split(","):
            if _CSS_SELECTOR.match(selector.strip()) is None:
                return False
        for declaration in match.group(2).split(";"):
            if not declaration.strip():
                continue
            name, colon, value = declaration.partition(":")
            if not colon:
                return False
            name, value = name.strip(), value.strip()
            if _CSS_PROPERTY.match(name) is None or name not in CSS_PROPERTIES \
                    or _CSS_VALUE.match(value) is None:
                return False
        position = match.end()
        while position < len(text) and text[position].isspace():
            position += 1
    return True


def _style_check(node, state):
    """Remove every `<style>` that is not entirely the safe CSS subset."""
    kept = []
    for child in node.children:
        if isinstance(child, _Node):
            if child.name == "style":
                css = "".join(item for item in child.children if isinstance(item, str))
                if (any(isinstance(item, _Node) for item in child.children)
                        or not safe_css(css)):
                    state["removed_elements"] += 1
                    continue
            _style_check(child, state)
        kept.append(child)
    node.children = kept


def _write(node, out):
    out.append("<" + node.name)
    for key, value in node.attributes:
        out.append(' %s="%s"' % (key, _escape(value, quote=True)))
    if not node.children:
        out.append("/>")
        return
    out.append(">")
    for child in node.children:
        if isinstance(child, _Node):
            _write(child, out)
        else:
            out.append(_escape(child))
    out.append("</%s>" % node.name)
