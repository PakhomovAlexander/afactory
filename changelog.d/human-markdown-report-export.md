- `af task output --format human-markdown` writes a readable presentation of a Document or
  report for a person: its sections with lists and examples, sources and citations, ordinary
  punctuation and non-ASCII text as written, the Task's result in plain words
  (`accepted`, `changes requested`, `incomplete`) and the reviewer's summary when an exactly
  matching receipt exists, recorded under the Task revision's own plan, verifier policy and
  source Manifest, and whose verifier received any captured sources; stale evidence refuses the
  export. Its closing note names only the checks and review that actually judged the canonical
  report. It is derived from the draft that renders exactly to the recorded report and is never
  recorded or reviewed itself. Authored Markdown, HTML and links stay text; an authored example
  keeps a fence line indented four columns, or padded by a no-break or other non-ASCII space,
  as content. Only ASCII spaces and tabs are layout: a no-break, em or ideographic space stays
  as written in prose, lists, headings and after the indentation of an indented example. A
  captured source's link opens exactly its captured URL: a query such as `?q=&copy;` is never
  decoded to `©`.
  `--format markdown` and `--format json` are unchanged
  ([ADR-0151](docs/adr/0151-export-a-human-presentation-of-a-report-beside-its-canonical-markdown.md)).
