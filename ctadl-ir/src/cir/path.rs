//! The path algebra: normal form and classification of access paths.
//!
//! This module is the one implementation of the equations of `docs/ir.md` (S2, S3). Every other
//! place that composes or splits paths (`facts::Path`, the index's `PathSet`, model ports, front
//! ends) calls into it or is tested to agree with it.
//!
//! A path is a sequence of [`PathSegment`]s. Its *normal form* applies these equations:
//!
//! - Offsets add, and zero does nothing: `p.[i].[j] = p.[i+j]`, `p.[0] = p`.
//! - `#value` absorbs itself and adjacent offsets: `p.#value.#value = p.#value`,
//!   `p.[k].#value = p.#value`, `p.#value.[k] = p.#value`.
//! - Fields and `deref` obey no equations.
//!
//! A normal-form path is exactly one of ([`classify`], checked in this order):
//!
//! - [`PathClass::Void`]: a field or `deref` follows `#value`. It denotes nothing.
//! - [`PathClass::Address`]: it ends in an offset. It holds no labels of its own; its labels are
//!   those of the path before the trailing offset.
//! - [`PathClass::Location`]: everything else. A cell carrying labels.

use crate::cir::{Offset, PathSegment};

/// What a normal-form path denotes (S3).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PathClass {
    /// A field or `deref` after `#value`: holds nothing, propagates nothing.
    Void,
    /// Ends in an offset: not a cell; its labels are its base's.
    Address,
    /// A cell carrying labels.
    Location,
}

/// Appends `seg` to the normal-form path `out`, keeping it in normal form.
///
/// This is the single step every normalizing operation is built from: [`normalize`] folds it
/// over a sequence, and [`concat()`] folds it over the suffix starting from the prefix.
pub fn push_normalized(out: &mut Vec<PathSegment>, seg: PathSegment) {
    match seg {
        PathSegment::Offset(Offset(k)) => match out.last_mut() {
            // `#value.[k] = #value`.
            Some(PathSegment::Value) => {}
            // `[i].[j] = [i+j]`, and a zero sum vanishes.
            Some(PathSegment::Offset(Offset(prev))) => {
                *prev = prev.wrapping_add(k);
                if *prev == 0 {
                    out.pop();
                }
            }
            _ => {
                if k != 0 {
                    out.push(PathSegment::Offset(Offset(k)));
                }
            }
        },
        PathSegment::Value => {
            // `[k].#value = #value`. The path is normal, so at most one offset precedes, and the
            // segment before it is not `#value` (that offset would have been absorbed).
            if matches!(out.last(), Some(PathSegment::Offset(_))) {
                out.pop();
            }
            // `#value.#value = #value`.
            if !matches!(out.last(), Some(PathSegment::Value)) {
                out.push(PathSegment::Value);
            }
        }
        seg @ (PathSegment::Field(_) | PathSegment::Deref) => out.push(seg),
    }
}

/// The normal form of `segments` (S2). Idempotent.
pub fn normalize(segments: impl IntoIterator<Item = PathSegment>) -> Vec<PathSegment> {
    let mut out = Vec::new();
    for seg in segments {
        push_normalized(&mut out, seg);
    }
    out
}

/// True if `segments` is already in normal form.
pub fn is_normal(segments: &[PathSegment]) -> bool {
    normalize(segments.iter().cloned()) == segments
}

/// Classifies a normal-form path (S3): void, else address, else location.
pub fn classify(segments: &[PathSegment]) -> PathClass {
    if let Some(v) = segments
        .iter()
        .position(|s| matches!(s, PathSegment::Value))
        && segments[v + 1..]
            .iter()
            .any(|s| matches!(s, PathSegment::Field(_) | PathSegment::Deref))
    {
        return PathClass::Void;
    }
    match segments.last() {
        Some(PathSegment::Offset(_)) => PathClass::Address,
        _ => PathClass::Location,
    }
}

/// True if the normal-form path is void.
#[inline]
pub fn is_void(segments: &[PathSegment]) -> bool {
    classify(segments) == PathClass::Void
}

/// True if the normal-form path is a location.
#[inline]
pub fn is_location(segments: &[PathSegment]) -> bool {
    classify(segments) == PathClass::Location
}

/// The normal form of `prefix · suffix`, where `prefix` is in normal form. Only the junction can
/// need rewriting.
pub fn concat(prefix: &[PathSegment], suffix: &[PathSegment]) -> Vec<PathSegment> {
    let mut out = prefix.to_vec();
    for seg in suffix {
        push_normalized(&mut out, seg.clone());
    }
    out
}

/// The suffix `t` with `concat(prefix, t) == path`, both in normal form, if `prefix` is a prefix
/// of `path` in the sense the semantics needs (S6, S8).
///
/// Every segment of `prefix` but the last must match `path` exactly. When the last segment of
/// `prefix` is an offset `[m]`, the address it names is rebased: against an offset `[n]` in
/// `path` the suffix starts `[n-m]`, and against anything else it starts `[-m]`. So `x.[8]` is a
/// prefix of `x.f` with suffix `[-8].f`, since `x.[8].[-8].f = x.f`.
///
/// When several suffixes would do (a prefix ending in `#value` absorbs offsets and `#value`
/// itself), the shortest is returned.
pub fn strip_prefix(path: &[PathSegment], prefix: &[PathSegment]) -> Option<Vec<PathSegment>> {
    let Some((last, init)) = prefix.split_last() else {
        return Some(path.to_vec());
    };
    if path.len() < init.len() || path[..init.len()] != *init {
        return None;
    }
    let rest = &path[init.len()..];
    match last {
        PathSegment::Offset(Offset(m)) => {
            let t = concat(&normalize([PathSegment::offset(m.wrapping_neg())]), rest);
            // For normal-form inputs the rebased suffix always reproduces `path`; checking keeps
            // the function sound on anything else.
            (concat(prefix, &t) == path).then_some(t)
        }
        seg => {
            let (head, tail) = rest.split_first()?;
            (head == seg).then(|| tail.to_vec())
        }
    }
}

/// Rewrites an assignment whose destination is an address, `p.[k] = e`, into `p = e.[-k]` (S5).
/// Returns the new destination and source paths, both in normal form. A destination that is not
/// an address is returned unchanged.
///
/// Both inputs must be in normal form. A normal-form address has exactly one trailing offset and
/// the segment before it is never `#value`, so one step suffices: the new destination is not an
/// address.
pub fn rewrite_address_dest(
    dest: &[PathSegment],
    src: &[PathSegment],
) -> (Vec<PathSegment>, Vec<PathSegment>) {
    match dest.split_last() {
        Some((PathSegment::Offset(Offset(k)), init)) => {
            debug_assert!(classify(init) != PathClass::Address);
            (
                init.to_vec(),
                concat(src, &[PathSegment::Offset(Offset(k.wrapping_neg()))]),
            )
        }
        _ => (dest.to_vec(), src.to_vec()),
    }
}

/// The address-to-base step of S3: `labels(p.[k]) = labels(p)`. For an address returns the path
/// before its trailing offset; any other path is returned as is.
pub fn location_of(segments: &[PathSegment]) -> &[PathSegment] {
    match segments.split_last() {
        Some((PathSegment::Offset(_), init)) => init,
        _ => segments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str) -> PathSegment {
        PathSegment::field(name)
    }
    fn o(k: i64) -> PathSegment {
        PathSegment::offset(k)
    }
    const D: PathSegment = PathSegment::Deref;
    const V: PathSegment = PathSegment::Value;

    #[test]
    fn equations() {
        assert_eq!(normalize([o(1), o(2)]), vec![o(3)]);
        assert_eq!(normalize([f("x"), o(0)]), vec![f("x")]);
        assert_eq!(normalize([o(3), o(-3)]), vec![]);
        assert_eq!(normalize([V, V]), vec![V]);
        assert_eq!(normalize([f("p"), o(8), V]), vec![f("p"), V]);
        assert_eq!(normalize([V, o(8)]), vec![V]);
        assert_eq!(normalize([o(8), V, o(4), V]), vec![V]);
        assert_eq!(normalize([f("a"), f("b")]), vec![f("a"), f("b")]);
        assert_eq!(normalize([D, D]), vec![D, D]);
        // A field named `deref` is not the deref segment.
        assert_ne!(normalize([f("deref")]), normalize([D]));
    }

    #[test]
    fn classification() {
        assert_eq!(classify(&[]), PathClass::Location);
        assert_eq!(classify(&[f("x")]), PathClass::Location);
        assert_eq!(classify(&[D]), PathClass::Location);
        assert_eq!(classify(&[V]), PathClass::Location);
        assert_eq!(classify(&[o(8)]), PathClass::Address);
        assert_eq!(classify(&[f("x"), o(8)]), PathClass::Address);
        assert_eq!(classify(&[V, f("x")]), PathClass::Void);
        assert_eq!(classify(&[V, D]), PathClass::Void);
        // Void is checked before address.
        assert_eq!(classify(&[V, D, o(8)]), PathClass::Void);
    }

    #[test]
    fn strip_and_concat() {
        assert_eq!(
            strip_prefix(&[f("a"), f("b")], &[f("a")]),
            Some(vec![f("b")])
        );
        assert_eq!(strip_prefix(&[f("a")], &[f("b")]), None);
        assert_eq!(strip_prefix(&[o(8), D], &[o(4)]), Some(vec![o(4), D]));
        assert_eq!(strip_prefix(&[o(8), D], &[o(8)]), Some(vec![D]));
        assert_eq!(strip_prefix(&[f("x")], &[o(8)]), Some(vec![o(-8), f("x")]));
        assert_eq!(strip_prefix(&[], &[o(8)]), Some(vec![o(-8)]));
        assert_eq!(strip_prefix(&[V], &[o(8)]), Some(vec![V]));
        assert_eq!(strip_prefix(&[f("a")], &[f("a"), f("b")]), None);
    }

    #[test]
    fn address_dest() {
        assert_eq!(rewrite_address_dest(&[o(8)], &[]), (vec![], vec![o(-8)]));
        assert_eq!(rewrite_address_dest(&[o(8)], &[o(8)]), (vec![], vec![]));
        assert_eq!(
            rewrite_address_dest(&[f("f"), o(8)], &[D]),
            (vec![f("f")], vec![D, o(-8)])
        );
        assert_eq!(
            rewrite_address_dest(&[f("f")], &[D]),
            (vec![f("f")], vec![D])
        );
    }

    /// A small deterministic generator, so the property tests need no extra dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn segment(&mut self) -> PathSegment {
            match self.next() % 6 {
                0 => f("a"),
                1 => f("b"),
                2 => D,
                3 => V,
                _ => o((self.next() % 5) as i64 * 4 - 8),
            }
        }
        fn path(&mut self) -> Vec<PathSegment> {
            let n = self.next() % 6;
            (0..n).map(|_| self.segment()).collect()
        }
    }

    #[test]
    fn properties() {
        let mut g = Lcg(0x5eed);
        for _ in 0..20_000 {
            let p = g.path();
            let q = g.path();
            let np = normalize(p.clone());
            // Idempotence.
            assert_eq!(normalize(np.clone()), np, "{p:?}");
            assert!(is_normal(&np));
            // Normalization respects concatenation.
            let nq = normalize(q.clone());
            let whole = normalize(p.iter().chain(q.iter()).cloned());
            assert_eq!(concat(&np, &nq), whole, "{p:?} · {q:?}");
            assert_eq!(concat(&np, &q), whole, "{p:?} · {q:?}");
            // No offset survives next to `#value`, no zero offset, no adjacent offsets, no
            // repeated `#value`.
            for w in np.windows(2) {
                assert!(
                    !matches!(
                        (&w[0], &w[1]),
                        (PathSegment::Offset(_), PathSegment::Offset(_))
                            | (PathSegment::Offset(_), PathSegment::Value)
                            | (PathSegment::Value, PathSegment::Offset(_))
                            | (PathSegment::Value, PathSegment::Value)
                    ),
                    "{np:?}"
                );
            }
            assert!(!np.contains(&o(0)));
            // Classification is total and in order.
            let c = classify(&np);
            let has_void = np.iter().position(|s| *s == V).is_some_and(|i| {
                np[i + 1..]
                    .iter()
                    .any(|s| matches!(s, PathSegment::Field(_) | PathSegment::Deref))
            });
            match c {
                PathClass::Void => assert!(has_void),
                PathClass::Address => {
                    assert!(!has_void && matches!(np.last(), Some(PathSegment::Offset(_))))
                }
                PathClass::Location => {
                    assert!(!has_void && !matches!(np.last(), Some(PathSegment::Offset(_))))
                }
            }
            // `strip_prefix ∘ concat` round trip.
            let joined = concat(&np, &nq);
            let t = strip_prefix(&joined, &np)
                .unwrap_or_else(|| panic!("{np:?} is a prefix of {joined:?}"));
            assert_eq!(concat(&np, &t), joined, "{np:?} · {t:?}");
            // And `strip_prefix` is always sound when it answers.
            if let Some(t) = strip_prefix(&nq, &np) {
                assert_eq!(concat(&np, &t), nq, "{np:?} · {t:?} vs {nq:?}");
            }
        }
    }
}
