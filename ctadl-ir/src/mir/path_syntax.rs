//! The one path grammar: parser and printer for [`PathSegment`] sequences.
//!
//! A path is a sequence of [`PathSegment`]s, each a field name, a numeric offset, `deref` or
//! `#value`; an access path ([`crate::mir::AccessPath`]) prints as its variable followed by its path. This
//! module owns the *only* textual encoding of that sequence. The fact store's on-disk columns,
//! model-generator ports, the IR's `Display` impls, and the test DSLs all go through here, so
//! printing and parsing are exact inverses and a malformed path fails loudly instead of silently
//! mutating.
//!
//! ```text
//! path    := segment*                      -- "" is the empty path
//! segment := '.' ( offset | 'deref' | '#value' | symbol )
//!                                          -- a leading '.' is required before every segment
//! offset  := '[' ('+'|'-')? DIGIT+ ']'     -- decimal i64, nothing else
//! symbol  := one or more chars, up to the next UNESCAPED '.',
//!            and NOT beginning with an unescaped '['
//! escape  := '\' ANY  ->  the literal char   ( '\.' '\[' '\\' '\d' '\#' )
//! ```
//!
//! The unescaped words `deref` and `#value` are the [`PathSegment::Deref`] and
//! [`PathSegment::Value`] segments. A *field* with one of those names is written with its first
//! character escaped, `\deref` or `\#value`, so the four segment kinds never collide.
//!
//! Printing puts a `.` before each segment, writes offsets in decimal, and escapes `\`, `.`, and
//! a **leading** `[` in a symbol, and the first character of a field named `deref` or `#value`. So `Symbol("[]")` prints as `.\[]` and `Symbol("[_elem_]")` as
//! `.\[_elem_]` -- the frontends' synthetic array-element fields keep their bracketed names and
//! survive the round trip as symbols rather than being reinterpreted as offsets.
//!
//! Note that `Symbol("")` has no spelling in this grammar: an empty segment is an error and no
//! escape produces nothing. [`write_segment`] `debug_assert!`s against it.

use std::fmt;

use internment::ArcIntern;

use crate::mir::{Offset, PathSegment};

/// The spelling of [`PathSegment::Deref`].
pub const DEREF: &str = "deref";
/// The spelling of [`PathSegment::Value`].
pub const VALUE: &str = "#value";

/// Where and why a string is not a path. `at` is a byte offset into the string that
/// was parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSyntaxError {
    pub at: usize,
    pub kind: PathSyntaxErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSyntaxErrorKind {
    /// Text before the first `.`, i.e. a segment not preceded by `.`.
    MissingLeadingDot,
    /// `..`, or a trailing `.`.
    EmptySegment,
    /// `[42` with no closing `]`.
    UnterminatedOffset,
    /// `[foo]`, `[0x2a]`, `[]` -- carries the bracket contents.
    InvalidOffset(String),
    /// The path ends in a lone `\`.
    TrailingEscape,
}

impl fmt::Display for PathSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid access path at byte {}: {}", self.at, self.kind)
    }
}

impl fmt::Display for PathSyntaxErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSyntaxErrorKind::MissingLeadingDot => {
                write!(f, "expected '.' before an access-path segment")
            }
            PathSyntaxErrorKind::EmptySegment => write!(f, "empty access-path segment"),
            PathSyntaxErrorKind::UnterminatedOffset => {
                write!(f, "unterminated offset: expected ']'")
            }
            PathSyntaxErrorKind::InvalidOffset(contents) => write!(
                f,
                "'[{contents}]' is not a decimal offset; write '\\[{contents}]' for a field named \
                 '[{contents}]'"
            ),
            PathSyntaxErrorKind::TrailingEscape => {
                write!(f, "access path ends in a lone '\\'")
            }
        }
    }
}

impl std::error::Error for PathSyntaxError {}

/// Parse one segment, WITHOUT its leading `.`.
///
/// `at` offsets in any returned error are relative to `s`; use [`parse_segments`] when you need
/// them relative to a whole path.
pub fn parse_segment(s: &str) -> Result<PathSegment, PathSyntaxError> {
    let (seg, rest_at) = parse_segment_at(s, 0)?;
    if rest_at != s.len() {
        // A `.` inside `s` means this is not one segment. Report it where the caller can see it.
        return Err(PathSyntaxError {
            at: rest_at,
            kind: PathSyntaxErrorKind::MissingLeadingDot,
        });
    }
    Ok(seg)
}

/// Parse a whole path into segments, in order.
///
/// Does no normalization: the segments are returned as written. [`parse_path`] normalizes.
pub fn parse_segments(s: &str) -> Result<Vec<PathSegment>, PathSyntaxError> {
    let mut segments = Vec::new();
    let mut at = 0usize;
    while at < s.len() {
        // Every segment must be introduced by a '.'.
        if s.as_bytes()[at] != b'.' {
            return Err(PathSyntaxError {
                at,
                kind: PathSyntaxErrorKind::MissingLeadingDot,
            });
        }
        at += 1;
        let (seg, next) = parse_segment_at(s, at)?;
        segments.push(seg);
        at = next;
    }
    Ok(segments)
}

/// Parse a whole path and put it in normal form ([`crate::mir::path::normalize`]). This is what
/// every consumer of textual paths should call: paths are compared only in normal form.
pub fn parse_path(s: &str) -> Result<Vec<PathSegment>, PathSyntaxError> {
    parse_segments(s).map(crate::mir::path::normalize)
}

/// Parses the segment beginning at byte `at` (the leading `.` already consumed) and returns it
/// with the byte offset just past its end -- which is either `s.len()` or the `.` that starts the
/// next segment.
fn parse_segment_at(s: &str, at: usize) -> Result<(PathSegment, usize), PathSyntaxError> {
    let bytes = s.as_bytes();
    if at >= bytes.len() || bytes[at] == b'.' {
        return Err(PathSyntaxError {
            at,
            kind: PathSyntaxErrorKind::EmptySegment,
        });
    }

    if bytes[at] == b'[' {
        // An unescaped '[' at segment start is an offset, always. A field name that really begins
        // with '[' is written '\['.
        let close = match s[at..].find(']') {
            Some(i) => at + i,
            None => {
                return Err(PathSyntaxError {
                    at,
                    kind: PathSyntaxErrorKind::UnterminatedOffset,
                });
            }
        };
        let contents = &s[at + 1..close];
        let value = parse_i64(contents).ok_or_else(|| PathSyntaxError {
            at,
            kind: PathSyntaxErrorKind::InvalidOffset(contents.to_string()),
        })?;
        // `]` must end the segment: '.[1]x' is not a path.
        let end = close + 1;
        if end < bytes.len() && bytes[end] != b'.' {
            return Err(PathSyntaxError {
                at: end,
                kind: PathSyntaxErrorKind::MissingLeadingDot,
            });
        }
        return Ok((PathSegment::Offset(Offset(value)), end));
    }

    // A symbol runs to the next unescaped '.'. `escaped_any` records whether any character was
    // escaped, so `\deref` is a field while `deref` is the segment.
    let mut name = String::new();
    let mut escaped_any = false;
    let mut i = at;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => break,
            b'\\' => {
                let next = i + 1;
                if next >= bytes.len() {
                    return Err(PathSyntaxError {
                        at: i,
                        kind: PathSyntaxErrorKind::TrailingEscape,
                    });
                }
                // `\` escapes whatever follows, which may be multi-byte.
                escaped_any = true;
                let ch = s[next..].chars().next().expect("non-empty remainder");
                name.push(ch);
                i = next + ch.len_utf8();
            }
            _ => {
                let ch = s[i..].chars().next().expect("non-empty remainder");
                name.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    let seg = match name.as_str() {
        DEREF if !escaped_any => PathSegment::Deref,
        VALUE if !escaped_any => PathSegment::Value,
        _ => PathSegment::Field(ArcIntern::from(name.as_str())),
    };
    Ok((seg, i))
}

/// Decimal `i64` with an optional sign, and nothing else -- no hex, no whitespace, no `_`.
fn parse_i64(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// Print one segment, WITHOUT a leading `.`.
pub fn write_segment(out: &mut String, seg: &PathSegment) {
    match seg {
        PathSegment::Offset(Offset(value)) => {
            use fmt::Write as _;
            let _ = write!(out, "[{value}]");
        }
        PathSegment::Deref => out.push_str(DEREF),
        PathSegment::Value => out.push_str(VALUE),
        PathSegment::Field(symbol) => {
            let name: &str = symbol.as_ref();
            debug_assert!(
                !name.is_empty(),
                "Field(\"\") has no spelling in the path grammar"
            );
            // A field spelled like a segment keyword escapes its first character.
            let keyword = name == DEREF || name == VALUE;
            for (i, ch) in name.char_indices() {
                match ch {
                    _ if i == 0 && keyword => {
                        out.push('\\');
                        out.push(ch);
                    }
                    '\\' | '.' => {
                        out.push('\\');
                        out.push(ch);
                    }
                    // Only a *leading* '[' is ambiguous with an offset; inside a name it is not.
                    '[' if i == 0 => {
                        out.push('\\');
                        out.push('[');
                    }
                    _ => out.push(ch),
                }
            }
        }
    }
}

/// Print a global's name as it follows the `$` of `$name`: escaped by the field rules, so a
/// name with a `.` in it (`g$util.c`) prints as `g$util\.c` and parses back to one name.
pub fn write_symbol(f: &mut impl fmt::Write, name: &str) -> fmt::Result {
    let mut out = String::new();
    write_segment(&mut out, &PathSegment::Field(ArcIntern::from(name)));
    f.write_str(&out)
}

/// Parses a global access path, `$name` followed by a path, into the global's name and the path
/// in normal form. Returns `Ok(None)` when `s` does not begin with `$`. The name follows the
/// field rules ([`write_symbol`]): it runs to the next unescaped `.`.
pub fn parse_global_access_path(
    s: &str,
) -> Result<Option<(ArcIntern<str>, Vec<PathSegment>)>, PathSyntaxError> {
    let Some(rest) = s.strip_prefix('$') else {
        return Ok(None);
    };
    let (seg, end) =
        parse_segment_at(rest, 0).map_err(|e| PathSyntaxError { at: e.at + 1, ..e })?;
    let name = match seg {
        PathSegment::Field(name) => name,
        PathSegment::Deref => ArcIntern::from(DEREF),
        PathSegment::Value => ArcIntern::from(VALUE),
        PathSegment::Offset(_) => {
            return Err(PathSyntaxError {
                at: 1,
                kind: PathSyntaxErrorKind::InvalidOffset(rest[..end].to_string()),
            });
        }
    };
    let path = parse_path(&rest[end..]).map_err(|e| PathSyntaxError {
        at: e.at + 1 + end,
        ..e
    })?;
    Ok(Some((name, path)))
}

/// Print one segment, WITHOUT a leading `.`.
pub fn segment_to_string(seg: &PathSegment) -> String {
    let mut out = String::new();
    write_segment(&mut out, seg);
    out
}

/// Print a whole path: a `.` before each segment. The empty path prints as `""`.
pub fn write_path<'a>(out: &mut String, segs: impl IntoIterator<Item = &'a PathSegment>) {
    for seg in segs {
        out.push('.');
        write_segment(out, seg);
    }
}

/// Print a whole path.
pub fn path_to_string<'a>(segs: impl IntoIterator<Item = &'a PathSegment>) -> String {
    let mut out = String::new();
    write_path(&mut out, segs);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str) -> PathSegment {
        PathSegment::field(name)
    }

    fn off(value: i64) -> PathSegment {
        PathSegment::offset(value)
    }

    fn parse_ok(s: &str) -> Vec<PathSegment> {
        parse_segments(s).unwrap_or_else(|e| panic!("{s:?} should parse, got {e}"))
    }

    fn parse_err(s: &str) -> PathSyntaxError {
        parse_segments(s).unwrap_err()
    }

    #[test]
    fn empty_path() {
        assert_eq!(parse_ok(""), vec![]);
        assert_eq!(path_to_string(&[]), "");
    }

    #[test]
    fn symbols() {
        assert_eq!(parse_ok(".foo"), vec![sym("foo")]);
        assert_eq!(parse_ok(".foo.bar"), vec![sym("foo"), sym("bar")]);
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_ok(".[42]"), vec![off(42)]);
        assert_eq!(parse_ok(".[-8]"), vec![off(-8)]);
        assert_eq!(parse_ok(".[+8]"), vec![off(8)]);
        assert_eq!(parse_ok(".[0]"), vec![off(0)]);
    }

    #[test]
    fn mixed() {
        assert_eq!(
            parse_ok(".foo.[42].bar"),
            vec![sym("foo"), off(42), sym("bar")]
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(parse_ok(r".a\.b"), vec![sym("a.b")]);
        assert_eq!(parse_ok(r".\[]"), vec![sym("[]")]);
        assert_eq!(parse_ok(r".\[_elem_]"), vec![sym("[_elem_]")]);
        assert_eq!(parse_ok(r".\[3]"), vec![sym("[3]")]);
        assert_eq!(parse_ok(r".a\\b"), vec![sym(r"a\b")]);
        // A '[' that is not at segment start needs no escape, but tolerates one.
        assert_eq!(parse_ok(".a[3]"), vec![sym("a[3]")]);
    }

    #[test]
    fn error_missing_leading_dot() {
        let e = parse_err("foo");
        assert_eq!(e.kind, PathSyntaxErrorKind::MissingLeadingDot);
        assert_eq!(e.at, 0);
    }

    #[test]
    fn error_empty_segment() {
        let e = parse_err("..a");
        assert_eq!(e.kind, PathSyntaxErrorKind::EmptySegment);
        assert_eq!(e.at, 1);

        let e = parse_err(".a.");
        assert_eq!(e.kind, PathSyntaxErrorKind::EmptySegment);
        assert_eq!(e.at, 3);
    }

    #[test]
    fn error_invalid_offset() {
        let e = parse_err(".[foo]");
        assert_eq!(e.kind, PathSyntaxErrorKind::InvalidOffset("foo".into()));
        assert_eq!(e.at, 1);

        let e = parse_err(".[]");
        assert_eq!(e.kind, PathSyntaxErrorKind::InvalidOffset("".into()));
        assert_eq!(e.at, 1);

        let e = parse_err(".[0x2a]");
        assert_eq!(e.kind, PathSyntaxErrorKind::InvalidOffset("0x2a".into()));
        assert_eq!(e.at, 1);

        let e = parse_err(".[*]");
        assert_eq!(e.kind, PathSyntaxErrorKind::InvalidOffset("*".into()));
        assert_eq!(e.at, 1);
    }

    #[test]
    fn error_unterminated_offset() {
        let e = parse_err(".[42");
        assert_eq!(e.kind, PathSyntaxErrorKind::UnterminatedOffset);
        assert_eq!(e.at, 1);
    }

    #[test]
    fn error_trailing_escape() {
        // `at` points at the offending backslash, as `InvalidOffset`'s points at its `[`.
        let e = parse_err(r".a\");
        assert_eq!(e.kind, PathSyntaxErrorKind::TrailingEscape);
        assert_eq!(e.at, 2);
    }

    #[test]
    fn error_junk_after_offset() {
        let e = parse_err(".[1]x");
        assert_eq!(e.kind, PathSyntaxErrorKind::MissingLeadingDot);
        assert_eq!(e.at, 4);
    }

    #[test]
    fn keywords() {
        assert_eq!(parse_ok(".deref"), vec![PathSegment::Deref]);
        assert_eq!(parse_ok(".#value"), vec![PathSegment::Value]);
        assert_eq!(parse_ok(r".\deref"), vec![sym("deref")]);
        assert_eq!(parse_ok(r".\#value"), vec![sym("#value")]);
        assert_eq!(parse_ok(".derefx"), vec![sym("derefx")]);
        assert_eq!(segment_to_string(&sym("deref")), r"\deref");
        assert_eq!(segment_to_string(&sym("#value")), r"\#value");
        assert_eq!(segment_to_string(&PathSegment::Deref), "deref");
        assert_eq!(segment_to_string(&PathSegment::Value), "#value");
    }

    #[test]
    fn parse_path_normalizes() {
        assert_eq!(
            parse_path(".[1].[2].f.[0].[8].#value.[4]").unwrap(),
            vec![off(3), sym("f"), PathSegment::Value]
        );
    }

    #[test]
    fn printing_escapes() {
        assert_eq!(segment_to_string(&sym("[]")), r"\[]");
        assert_eq!(segment_to_string(&sym("[_elem_]")), r"\[_elem_]");
        assert_eq!(segment_to_string(&sym("[3]")), r"\[3]");
        assert_eq!(segment_to_string(&sym("a.b")), r"a\.b");
        assert_eq!(segment_to_string(&sym(r"a\b")), r"a\\b");
        assert_eq!(segment_to_string(&sym("a[3]")), "a[3]");
        assert_eq!(segment_to_string(&off(42)), "[42]");
        assert_eq!(segment_to_string(&off(-8)), "[-8]");
    }

    #[test]
    fn round_trip() {
        let corpus: Vec<Vec<PathSegment>> = vec![
            vec![],
            vec![sym("foo")],
            vec![sym("foo"), sym("bar")],
            vec![off(42)],
            vec![off(-1)],
            vec![off(0)],
            vec![off(1), off(2)],
            vec![sym("[]")],
            vec![sym("[_elem_]")],
            vec![sym("[3]")],
            vec![sym("a.b")],
            vec![sym(r"a\b")],
            vec![sym("a[3]")],
            vec![sym("foo"), off(8), sym("deref")],
            vec![sym("foo"), off(8), PathSegment::Deref],
            vec![sym("#value")],
            vec![PathSegment::Value],
            vec![PathSegment::Deref, sym("deref"), PathSegment::Value],
            vec![sym("*")],
        ];
        for segs in corpus {
            let printed = path_to_string(&segs);
            let reparsed = parse_segments(&printed)
                .unwrap_or_else(|e| panic!("{segs:?} printed as {printed:?} which failed: {e}"));
            assert_eq!(reparsed, segs, "round trip through {printed:?}");
        }
    }

    #[test]
    fn parse_segment_rejects_dots() {
        assert_eq!(parse_segment("foo").unwrap(), sym("foo"));
        assert_eq!(parse_segment("[8]").unwrap(), off(8));
        assert_eq!(parse_segment(r"\[]").unwrap(), sym("[]"));
        assert_eq!(
            parse_segment("foo.bar").unwrap_err().kind,
            PathSyntaxErrorKind::MissingLeadingDot
        );
        assert_eq!(
            parse_segment("").unwrap_err().kind,
            PathSyntaxErrorKind::EmptySegment
        );
    }

    #[test]
    fn global_access_paths_round_trip() {
        for (text, name, path) in [
            ("$g$util\\.c.deref", "g$util.c", vec![PathSegment::Deref]),
            (
                "$libA.so::ram.[4096].deref",
                "libA",
                vec![sym("so::ram"), off(4096), PathSegment::Deref],
            ),
            (
                "$libA\\.so::ram.[4096].deref",
                "libA.so::ram",
                vec![off(4096), PathSegment::Deref],
            ),
            ("$Lcom/C;.f", "Lcom/C;", vec![sym("f")]),
        ] {
            let (n, p) = parse_global_access_path(text).unwrap().unwrap();
            assert_eq!((&*n, &p), (name, &path), "{text}");
            let mut printed = String::from("$");
            write_symbol(&mut printed, &n).unwrap();
            printed.push_str(&path_to_string(&p));
            assert_eq!(printed, text);
        }
        assert_eq!(parse_global_access_path("@p0.f"), Ok(None));
        assert!(parse_global_access_path("$").is_err());
    }
}
