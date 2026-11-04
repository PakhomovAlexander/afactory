- Add explicit immutable cache and checkpoint policies to Worker development companions. Same-slot
  admitted outputs can warm later Workers through recorded Warm Sets, with private writable clones,
  language-neutral compatibility keys, bounded capture and explicit miss/corruption outcomes.
  Retries preserve their original inputs; Worker caches never become Gate evidence. Companion and
  machine bindings now require version 2, Build Caches version 2 and Warm Sets version 3. See the
  [decision](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/2026-11-04-carry-declared-worker-caches-in-exact-warm-sets.md).
