pub mod graph;
pub mod index;
pub mod mir;
pub mod ssa;

// trait that is used everywhere
pub use index::idx::Idx;
pub use mir::*;

/// indexvec![elt, elt]
#[macro_export]
macro_rules! indexvec {
    ($expr:expr; $n:expr) => {
        IndexVec::from_raw(vec![$expr; $n])
    };
    ($($expr:expr),* $(,)?) => {
        IndexVec::from_raw(vec![$($expr),*])
    };
}

/// Generates a newtype for an index type using thex `Idx` trait.
/// This macro requires that your crate has a feature called "serde". If the crate feature is
/// enabled, it derives Serialize and Deserialize.
///
/// `newtype_index!(Node, u32)`
#[macro_export]
macro_rules! newtype_index {
    ($name:ident, $ty:ty) => {
        #[derive(Clone, Copy, Debug, Default, PartialOrd, Ord, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #[repr(transparent)]
        pub struct $name($ty);

        impl $name {
            pub const ZERO: $name = $name(0);
            pub const MAX: $name = $name(<$ty>::MAX);
        }

        impl Idx for $name {
            #[inline]
            fn new(idx: usize) -> Self {
                $name(<$ty>::try_from(idx).unwrap())
            }

            #[inline]
            fn index(self) -> usize {
                <usize>::try_from(self.0).unwrap()
            }
        }

        impl From<$ty> for $name {
            #[inline]
            fn from(id: $ty) -> Self {
                Self(id)
            }
        }

        impl From<$name> for $ty {
            #[inline]
            fn from(idx: $name) -> Self {
                idx.0
            }
        }

        impl $name {
            #[inline]
            pub fn get(self) -> $ty {
                self.0
            }
        }
    };
}
