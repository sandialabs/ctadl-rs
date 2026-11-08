//! [`Sources`], the operands of a [`StatementKind::Assign`](super::StatementKind::Assign).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};

use thin_vec::ThinVec;

use super::Exp;

/// The sources of an assignment: a sequence of expressions that derefs to `[Exp]`.
///
/// Nearly every assignment has exactly one source, so that one is held inline and any other
/// number on the heap. This keeps the type the size of one [`Exp`], which keeps
/// [`Statement`](super::Statement) small: statements are a large share of a program's memory.
///
/// Equality, hashing, ordering of elements, `Debug` and the serialized form are those of the
/// slice, whichever way it is held.
#[derive(Clone)]
pub struct Sources(Repr);

#[derive(Clone)]
enum Repr {
    One(Exp),
    /// Any length but one.
    Many(ThinVec<Exp>),
}

impl Sources {
    /// No sources.
    #[inline]
    pub fn new() -> Self {
        Sources(Repr::Many(ThinVec::new()))
    }

    /// The single source `e`.
    #[inline]
    pub const fn one(e: Exp) -> Self {
        Sources(Repr::One(e))
    }

    #[inline]
    pub fn as_slice(&self) -> &[Exp] {
        match &self.0 {
            Repr::One(e) => std::slice::from_ref(e),
            Repr::Many(v) => v,
        }
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [Exp] {
        match &mut self.0 {
            Repr::One(e) => std::slice::from_mut(e),
            Repr::Many(v) => v,
        }
    }

    /// Appends `e`.
    pub fn push(&mut self, e: Exp) {
        match &mut self.0 {
            Repr::Many(v) if v.is_empty() => self.0 = Repr::One(e),
            Repr::Many(v) => v.push(e),
            Repr::One(_) => {
                let Repr::One(first) = std::mem::replace(&mut self.0, Repr::Many(ThinVec::new()))
                else {
                    unreachable!()
                };
                let mut v = ThinVec::with_capacity(2);
                v.push(first);
                v.push(e);
                self.0 = Repr::Many(v);
            }
        }
    }

    /// Keeps the sources for which `keep` is true, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&Exp) -> bool) {
        let all = std::mem::take(self);
        *self = all.into_iter().filter(|e| keep(e)).collect();
    }
}

impl Default for Sources {
    #[inline]
    fn default() -> Self {
        Sources::new()
    }
}

impl Deref for Sources {
    type Target = [Exp];
    #[inline]
    fn deref(&self) -> &[Exp] {
        self.as_slice()
    }
}

impl DerefMut for Sources {
    #[inline]
    fn deref_mut(&mut self) -> &mut [Exp] {
        self.as_mut_slice()
    }
}

impl PartialEq for Sources {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Sources {}

impl Hash for Sources {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state)
    }
}

impl fmt::Debug for Sources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl From<Exp> for Sources {
    #[inline]
    fn from(e: Exp) -> Self {
        Sources::one(e)
    }
}

impl<const N: usize> From<[Exp; N]> for Sources {
    #[inline]
    fn from(es: [Exp; N]) -> Self {
        es.into_iter().collect()
    }
}

impl From<Vec<Exp>> for Sources {
    #[inline]
    fn from(es: Vec<Exp>) -> Self {
        es.into_iter().collect()
    }
}

impl FromIterator<Exp> for Sources {
    fn from_iter<I: IntoIterator<Item = Exp>>(iter: I) -> Self {
        let mut iter = iter.into_iter();
        let Some(first) = iter.next() else {
            return Sources::new();
        };
        let Some(second) = iter.next() else {
            return Sources::one(first);
        };
        let mut v = ThinVec::with_capacity(2 + iter.size_hint().0);
        v.push(first);
        v.push(second);
        v.extend(iter);
        Sources(Repr::Many(v))
    }
}

impl Extend<Exp> for Sources {
    fn extend<I: IntoIterator<Item = Exp>>(&mut self, iter: I) {
        for e in iter {
            self.push(e);
        }
    }
}

/// The owning iterator of [`Sources`].
pub struct IntoIter(std::iter::Chain<std::option::IntoIter<Exp>, thin_vec::IntoIter<Exp>>);

impl Iterator for IntoIter {
    type Item = Exp;
    #[inline]
    fn next(&mut self) -> Option<Exp> {
        self.0.next()
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl DoubleEndedIterator for IntoIter {
    #[inline]
    fn next_back(&mut self) -> Option<Exp> {
        self.0.next_back()
    }
}

impl IntoIterator for Sources {
    type Item = Exp;
    type IntoIter = IntoIter;
    fn into_iter(self) -> IntoIter {
        let (one, many) = match self.0 {
            Repr::One(e) => (Some(e), ThinVec::new()),
            Repr::Many(v) => (None, v),
        };
        IntoIter(one.into_iter().chain(many))
    }
}

impl<'a> IntoIterator for &'a Sources {
    type Item = &'a Exp;
    type IntoIter = std::slice::Iter<'a, Exp>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a mut Sources {
    type Item = &'a mut Exp;
    type IntoIter = std::slice::IterMut<'a, Exp>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Sources {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.as_slice())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Sources {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Sources;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence of expressions")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Sources, A::Error> {
                let Some(first) = seq.next_element::<Exp>()? else {
                    return Ok(Sources::new());
                };
                let Some(second) = seq.next_element::<Exp>()? else {
                    return Ok(Sources::one(first));
                };
                let mut v = ThinVec::with_capacity(2 + seq.size_hint().unwrap_or(0));
                v.push(first);
                v.push(second);
                while let Some(e) = seq.next_element()? {
                    v.push(e);
                }
                Ok(Sources(Repr::Many(v)))
            }
        }
        deserializer.deserialize_seq(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(i: i64) -> Exp {
        Exp::Int(i)
    }

    #[test]
    fn push_collect_and_retain_agree_with_a_vec() {
        let mut s = Sources::new();
        let mut v = Vec::new();
        for i in 0..4 {
            assert_eq!(&*s, &v[..]);
            s.push(int(i));
            v.push(int(i));
        }
        assert_eq!(s, v.iter().cloned().collect::<Sources>());
        s.retain(|e| matches!(e, Exp::Int(1)));
        assert_eq!(&*s, &[int(1)]);
        assert_eq!(s, Sources::one(int(1)));
        s.retain(|_| false);
        assert!(s.is_empty());
        let back: Vec<Exp> = Sources::from([int(1), int(2)]).into_iter().rev().collect();
        assert_eq!(back, vec![int(2), int(1)]);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serializes_as_a_sequence() {
        for n in 0..4 {
            let v: Vec<Exp> = (0..n).map(int).collect();
            let s: Sources = v.iter().cloned().collect();
            let bytes = bitcode::serialize(&s).unwrap();
            assert_eq!(bytes, bitcode::serialize(&v).unwrap());
            assert_eq!(bitcode::deserialize::<Sources>(&bytes).unwrap(), s);
        }
    }
}
