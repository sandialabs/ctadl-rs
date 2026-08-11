use std::collections::BTreeSet;
use std::hash::Hash;

/// A set with an explicit 'all' element representing the universe. Helpful for when one desires to
/// do set operations but the universe isn't known in advance, or computing the universe is an
/// uncommon case.
#[derive(Default, Clone, Debug)]
pub enum UniverseSet<T: Hash + Eq + Ord> {
    #[default]
    All,
    Explicit(BTreeSet<T>),
}

impl<T: Hash + Eq + Ord> UniverseSet<T> {
    #[inline]
    pub fn all() -> UniverseSet<T> {
        Self::All
    }

    #[inline]
    pub fn empty() -> UniverseSet<T> {
        Self::Explicit(BTreeSet::new())
    }

    /// Returns None for All and the is_empty of the explicit set otherwise
    pub fn is_empty(&self) -> Option<bool> {
        match self {
            UniverseSet::All => None,
            UniverseSet::Explicit(s) => Some(s.is_empty()),
        }
    }

    #[inline]
    pub fn intersect(self, other: UniverseSet<T>) -> Self {
        match (self, other) {
            (Self::All, r) | (r, Self::All) => r,
            (Self::Explicit(mut a), Self::Explicit(b)) => {
                a.retain(|x| b.contains(x));
                Self::Explicit(a)
            }
        }
    }

    #[inline]
    pub fn intersect_with(&mut self, other: UniverseSet<T>) {
        let this = std::mem::take(self);

        *self = match (this, other) {
            (Self::All, r) | (r, Self::All) => r,
            (Self::Explicit(mut a), Self::Explicit(b)) => {
                a.retain(|x| b.contains(x));
                Self::Explicit(a)
            }
        };
    }

    #[inline]
    pub fn union(self, other: UniverseSet<T>) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::Explicit(mut a), Self::Explicit(b)) => {
                a.extend(b);
                Self::Explicit(a)
            }
        }
    }

    #[inline]
    pub fn union_with(&mut self, other: UniverseSet<T>) {
        let this = std::mem::take(self);

        *self = match (this, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::Explicit(mut a), Self::Explicit(b)) => {
                a.extend(b);
                Self::Explicit(a)
            }
        };
    }

    /// Set difference `self \ other`.
    ///
    /// The left-hand side must be materialized (`Explicit`); computing
    /// `All \ other` would require enumerating the universe, which this
    /// type is deliberately agnostic about (callers materialize `All`
    /// against the known universe first — see `ModelGeneratorIngest`). An
    /// `All` left-hand side is a caller bug and yields the empty set.
    #[inline]
    pub fn difference(self, other: UniverseSet<T>) -> Self {
        match (self, other) {
            // Removing the entire universe leaves nothing.
            (_, Self::All) => Self::empty(),
            (Self::All, _) => {
                debug_assert!(false, "UniverseSet::difference: lhs must be materialized");
                Self::empty()
            }
            (Self::Explicit(mut a), Self::Explicit(b)) => {
                a.retain(|x| !b.contains(x));
                Self::Explicit(a)
            }
        }
    }

    /// In-place set difference; see [`Self::difference`].
    #[inline]
    pub fn difference_with(&mut self, other: UniverseSet<T>) {
        let this = std::mem::take(self);
        *self = this.difference(other);
    }
}

impl<T: Hash + Eq + Ord> From<BTreeSet<T>> for UniverseSet<T> {
    #[inline]
    fn from(s: BTreeSet<T>) -> Self {
        Self::Explicit(s)
    }
}

impl<T: Hash + Eq + Ord> FromIterator<T> for UniverseSet<T> {
    #[inline]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::Explicit(BTreeSet::from_iter(iter))
    }
}
