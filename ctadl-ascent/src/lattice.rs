/*!
Generic lattices for use in `ascent` lattice relations.
*/

use std::cmp::Ordering;

use ascent::lattice::Lattice;

/// A lattice that enforces a *functional dependency*: for a given key (the
/// non-lattice columns of a relation) there is at most one value.
///
/// The order is `Bottom < Value(_)`, with every distinct `Value` being a maximal,
/// pairwise-incomparable element — "infinite tops":
///
/// - [`Bottom`](Consistent::Bottom) is the identity for `join`. It represents "no
///   value yet" and is the default. In an `ascent` lattice relation it is never
///   actually materialized, since a key only exists once some value is inserted.
/// - [`Value`](Consistent::Value) holds a value. Once a key holds a `Value`, `join`
///   never moves it: a value is a fixed point we never leave.
///
/// `join` is therefore "sticky": `Bottom` adopts the incoming value, and a `Value`
/// keeps the one it already has. Under a genuine functional dependency every value
/// proposed for a key is equal, so this is exact and order-independent. If the
/// dependency is violated (two *different* values for one key), the first one
/// observed wins rather than the value collapsing away — we never fold back to
/// `Bottom`.
///
/// Note this means `join` is not a true least-upper-bound when two different values
/// meet (distinct `Value`s have no common upper bound — that is the whole point of
/// "infinite tops"). `ascent` only ever merges lattice columns through
/// [`Lattice::join_mut`], so this is well-defined and terminating for its purposes;
/// `meet`, by contrast, is a genuine greatest-lower-bound (two distinct values meet
/// at `Bottom`).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Consistent<T> {
    /// No value yet — the lattice bottom and the identity for `join`.
    #[default]
    Bottom,
    /// A value. Maximal: once reached, `join` never moves off it.
    Value(T),
}

impl<T> Consistent<T> {
    /// Returns the value, if any.
    pub fn value(&self) -> Option<&T> {
        match self {
            Consistent::Value(v) => Some(v),
            Consistent::Bottom => None,
        }
    }
}

impl<T: Eq> PartialOrd for Consistent<T> {
    /// `Bottom < Value(_)`; distinct values are incomparable.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Consistent::Bottom, Consistent::Bottom) => Some(Ordering::Equal),
            (Consistent::Bottom, Consistent::Value(_)) => Some(Ordering::Less),
            (Consistent::Value(_), Consistent::Bottom) => Some(Ordering::Greater),
            (Consistent::Value(a), Consistent::Value(b)) => {
                (a == b).then_some(Ordering::Equal)
            }
        }
    }
}

impl<T: Clone + Eq> Lattice for Consistent<T> {
    /// Greatest lower bound: distinct values meet at `Bottom`.
    fn meet_mut(&mut self, other: Self) -> bool {
        let met = match (&*self, other) {
            (Consistent::Bottom, _) => return false,
            (Consistent::Value(a), Consistent::Value(b)) if *a == b => return false,
            // `Value` met with `Bottom` or a different `Value` drops to `Bottom`.
            (Consistent::Value(_), _) => Consistent::Bottom,
        };
        *self = met;
        true
    }

    /// Sticky least upper bound: `Bottom` adopts `other`; a `Value` never moves.
    fn join_mut(&mut self, other: Self) -> bool {
        match self {
            // Already holding a value — never move off it.
            Consistent::Value(_) => false,
            Consistent::Bottom => match other {
                Consistent::Bottom => false,
                other @ Consistent::Value(_) => {
                    *self = other;
                    true
                }
            },
        }
    }
}
