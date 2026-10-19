//! The two glob forms of `[survey]` (ADR-0152).
//!
//! A *path* glob (`exclude`, `tests`, `decisions`) is anchored at the repository root and
//! matches by segment, like the routes of `.af/af.toml`: `**` spans any number of segments, `*`
//! any run within one, `?` one character, and `[...]` one character of a class (`[0-9]`,
//! `[!.]`). A *name* glob (`agent_authors`, `agent_trailers`, `release_tags`) matches a whole
//! text: `*` any run, `?` one character, everything else literally, so `*[bot]` matches a name
//! that ends in `[bot]`.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    Any,
    Star,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

/// The tokens of one segment of a path glob, or why it is not one.
fn segment_tokens(segment: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = segment.chars().peekable();
    while let Some(character) = chars.next() {
        tokens.push(match character {
            '*' => Token::Star,
            '?' => Token::Any,
            '[' => {
                let negated = matches!(chars.peek(), Some('!' | '^'));
                if negated {
                    chars.next();
                }
                let mut ranges = Vec::new();
                let mut closed = false;
                while let Some(start) = chars.next() {
                    if start == ']' && !ranges.is_empty() {
                        closed = true;
                        break;
                    }
                    let mut end = start;
                    if chars.peek() == Some(&'-') {
                        chars.next();
                        match chars.next() {
                            Some(']') | None => {
                                ranges.push((start, start));
                                ranges.push(('-', '-'));
                                closed = true;
                                break;
                            }
                            Some(last) => end = last,
                        }
                    }
                    if end < start {
                        return Err(format!("the class range `{start}-{end}` is reversed"));
                    }
                    ranges.push((start, end));
                }
                if !closed {
                    return Err(format!("`{segment}` opens a `[` class it never closes"));
                }
                Token::Class { negated, ranges }
            }
            other => Token::Literal(other),
        });
    }
    Ok(tokens)
}

fn name_tokens(pattern: &str) -> Vec<Token> {
    pattern
        .chars()
        .map(|character| match character {
            '*' => Token::Star,
            '?' => Token::Any,
            other => Token::Literal(other),
        })
        .collect()
}

fn token_matches(token: &Token, character: char) -> bool {
    match token {
        Token::Literal(expected) => *expected == character,
        Token::Any => true,
        Token::Star => false,
        Token::Class { negated, ranges } => {
            ranges
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&character))
                != *negated
        }
    }
}

/// Wildcard matching with one remembered star: linear in the text for each star.
fn tokens_match(tokens: &[Token], text: &[char]) -> bool {
    let (mut token, mut position) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while position < text.len() {
        if token < tokens.len()
            && tokens[token] != Token::Star
            && token_matches(&tokens[token], text[position])
        {
            token += 1;
            position += 1;
        } else if token < tokens.len() && tokens[token] == Token::Star {
            star = Some((token, position));
            token += 1;
        } else if let Some((star_token, star_position)) = star {
            token = star_token + 1;
            position = star_position + 1;
            star = Some((star_token, star_position + 1));
        } else {
            return false;
        }
    }
    tokens[token..].iter().all(|rest| *rest == Token::Star)
}

/// Whether `pattern` is a path glob this module can match.
pub(crate) fn validate_path_glob(pattern: &str) -> Result<(), String> {
    if pattern.starts_with('/') {
        return Err(format!(
            "`{pattern}` is anchored at the repository root and starts without `/`"
        ));
    }
    for segment in pattern.split('/') {
        if segment.is_empty() {
            return Err(format!("`{pattern}` has an empty segment"));
        }
        if segment != "**" {
            segment_tokens(segment).map_err(|error| format!("`{pattern}`: {error}"))?;
        }
    }
    Ok(())
}

/// Whether the encoded repository path `path` matches the path glob `pattern`. A pattern
/// [`validate_path_glob`] refuses matches nothing.
pub(crate) fn path_matches(pattern: &str, path: &str) -> bool {
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|skip| segments(rest, &path[skip..])),
            Some((head, rest)) => match path.split_first() {
                Some((first, remaining)) => {
                    let Ok(tokens) = segment_tokens(head) else {
                        return false;
                    };
                    let text: Vec<char> = first.chars().collect();
                    tokens_match(&tokens, &text) && segments(rest, remaining)
                }
                None => false,
            },
        }
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

/// Whether `text` matches the name glob `pattern` as a whole.
pub(crate) fn name_matches(pattern: &str, text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    tokens_match(&name_tokens(pattern), &text)
}

/// Whether any of `patterns` matches `path`.
pub(crate) fn any_path(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|pattern| path_matches(pattern, path))
}

/// Whether any of `patterns` matches `text`.
pub(crate) fn any_name(patterns: &[String], text: &str) -> bool {
    patterns.iter().any(|pattern| name_matches(pattern, text))
}
