//! Bounded matching of canonical repository path patterns.

/// Segment-wise glob: `**` spans any number of segments, `*` any run within a segment, `?` one
/// byte of the canonical encoded spelling (`af_contracts::encode_path`'s: `é` is two bytes, a
/// percent-encoded byte three). Anchored at the repository root; `path` is the canonical
/// (encoded) form.
///
/// Matched by dynamic programming, never by backtracking (ADR-0159 clause 14): a match costs at
/// most (pattern length + 1) × (path length + 1) cells, so `*a` thirty times then `b` against
/// thirty `a`s, which the recursive form took billions of steps to refuse, is refused in under
/// a thousand.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_match_cells(pattern, path).0
}

/// [`glob_matches`] and the cells it evaluated: one per pair of a pattern segment and a path
/// segment it compared, one per pair of a pattern byte and a text byte inside them.
pub fn glob_match_cells(pattern: &str, path: &str) -> (bool, u64) {
    let mut cells = 0_u64;
    let pattern: Vec<&[u8]> = pattern.split('/').map(str::as_bytes).collect();
    let path: Vec<&[u8]> = path.split('/').map(str::as_bytes).collect();
    // `reached[j]`: the pattern segments so far match the first `j` path segments.
    let mut reached = vec![false; path.len() + 1];
    reached[0] = true;
    for head in &pattern {
        let mut next = vec![false; path.len() + 1];
        let Some(first) = reached.iter().position(|reached| *reached) else {
            return (false, cells);
        };
        if *head == b"**" {
            // Any number of segments from the first prefix reached on.
            cells += (path.len() + 1 - first) as u64;
            next[first..].fill(true);
        } else {
            for j in first..path.len() {
                if reached[j] {
                    cells += 1;
                    next[j + 1] = segment_matches(head, path[j], &mut cells);
                }
            }
        }
        reached = next;
    }
    (reached[path.len()], cells)
}

/// One segment against one pattern segment: `*` a run, `?` one byte, anything else itself.
/// `reached[j]`: the pattern bytes so far match the first `j` text bytes; only the text bytes at
/// or after the first prefix reached are visited, and a pattern byte that reaches none ends it.
fn segment_matches(pattern: &[u8], text: &[u8], cells: &mut u64) -> bool {
    let mut reached = vec![false; text.len() + 1];
    reached[0] = true;
    for &expected in pattern {
        let Some(first) = reached.iter().position(|reached| *reached) else {
            return false;
        };
        let mut next = vec![false; text.len() + 1];
        if expected == b'*' {
            *cells += (text.len() + 1 - first) as u64;
            next[first..].fill(true);
        } else {
            for j in first..text.len() {
                *cells += 1;
                next[j + 1] = reached[j] && (expected == b'?' || expected == text[j]);
            }
        }
        reached = next;
    }
    reached[text.len()]
}
