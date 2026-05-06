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
