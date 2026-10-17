- The `PR report` check no longer accepts an `af task report` block GitHub does not render as
  the report: a block wrapped in a raw HTML block (`<pre>`, `<script>`, `<style>` or
  `<textarea>`, indented or not, closed or left open), whose markers are its text, and a
  pipeline line or round table, between visible markers, that sits in a code block, inside an
  HTML block, which a fence inside it does not end before its blank line, inside an HTML
  comment, or inside the quoted attribute value of a tag an HTML block leaves open, or a
  pipeline line that starts inside a code span opened on a paragraph line above it. A comment
  stays hidden from its `<!--`, in an HTML block or spanning the lines of a paragraph, across
  blank lines to the first `-->` of raw HTML. A raw block ends at the closing tag of any of the
  four names, a `<pre>` nested in another HTML block opens none, and a `<!--` in an attribute
  value, a code span or escaped hides nothing, as GitHub renders them; so do steps or a Task cell
  in a code span. The block-level tags that interrupt a paragraph are GitHub's: `<source>` is
  one, `<search>` is not (issue #193,
  [ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).
