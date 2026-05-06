use std::collections::{VecDeque, vec_deque};
use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::RangeBounds;

use crate::index::{idx::Idx, slice::IndexSlice};

/// An owned deque of `T`s, indexed by `I` rather than:e  by `usize`.
///
/// ## Why use this instead of a `VecDeque`?
///
/// An `IndexVecDeque` allows element access only via a specific associated index type, meaning
/// that trying to use the wrong index type (possibly accessing an invalid element) will fail at
/// compile time.
///
/// It also documents what the index is indexing: in a `HashMap<usize, Something>` it's not
/// immediately clear what the `usize` means, while a `HashMap<FieldIdx, Something>` makes it obvious.
///
/// While it's possible to use `u32` or `usize` directly for `I`, you almost certainly want to use
/// a newtype instead.
///
/// This allows to index the IndexVecDeque with the new index type.
///
#[derive(Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(transparent)]
pub struct IndexVecDeque<I: Idx, T> {
    pub raw: VecDeque<T>,
    _marker: PhantomData<fn(&I)>,
}

impl<I: Idx, T> IndexVecDeque<I, T> {
    /// Constructs a new, empty `IndexVec<I, T>`.
    #[inline]
    pub const fn new() -> Self {
        IndexVecDeque::from_raw(VecDeque::new())
    }

    /// Constructs a new `IndexVec<I, T>` from a `Vec<T>`.
    #[inline]
    pub const fn from_raw(raw: VecDeque<T>) -> Self {
        IndexVecDeque {
            raw,
            _marker: PhantomData,
        }
    }

    /// Constructs an `IndexVecDeque` with the given capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        IndexVecDeque::from_raw(VecDeque::with_capacity(capacity))
    }

    /// Creates a new vector with a copy of `elem` for each index in `universe`.
    ///
    /// Thus `IndexVec::from_elem(elem, &universe)` is equivalent to
    /// `IndexVec::<I, _>::from_elem_n(elem, universe.len())`. That can help
    /// type inference as it ensures that the resulting vector uses the same
    /// index type as `universe`, rather than something potentially surprising.
    ///
    /// For example, if you want to store data for each local in a MIR body,
    /// using `let mut uses = IndexVec::from_elem(vec![], &body.local_decls);`
    /// ensures that `uses` is an `IndexVec<Local, _>`, and thus can give
    /// better error messages later if one accidentally mismatches indices.
    #[inline]
    pub fn from_elem<S>(elem: T, universe: &IndexSlice<I, S>) -> Self
    where
        T: Clone,
    {
        IndexVecDeque::from_raw(vec![elem; universe.len()].into())
    }

    /// Creates a new IndexVec with n copies of the `elem`.
    #[inline]
    pub fn from_elem_n(elem: T, n: usize) -> Self
    where
        T: Clone,
    {
        IndexVecDeque::from_raw(vec![elem; n].into())
    }

    /// Create an `IndexVec` with `n` elements, where the value of each
    /// element is the result of `func(i)`. (The underlying vector will
    /// be allocated only once, with a capacity of at least `n`.)
    #[inline]
    pub fn from_fn_n(func: impl FnMut(I) -> T, n: usize) -> Self {
        // Allow the optimizer to elide the bounds checking when creating each index.
        let _ = I::new(n);
        IndexVecDeque::from_raw((0..n).map(I::new).map(func).collect())
    }

    #[inline]
    pub fn as_slices(&self) -> (&IndexSlice<I, T>, &IndexSlice<I, T>) {
        let (l, r) = self.raw.as_slices();
        (IndexSlice::from_raw(l), IndexSlice::from_raw(r))
    }

    #[inline]
    pub fn as_mut_slices(&mut self) -> (&mut IndexSlice<I, T>, &mut IndexSlice<I, T>) {
        let (l, r) = self.raw.as_mut_slices();
        (IndexSlice::from_raw_mut(l), IndexSlice::from_raw_mut(r))
    }

    /// Pushes an element to the back of the deque.
    #[inline]
    pub fn push_back(&mut self, d: T) {
        self.raw.push_back(d)
    }

    /// Pushes an element to the front of the deque.
    #[inline]
    pub fn push_front(&mut self, d: T) {
        self.raw.push_front(d)
    }

    #[inline]
    pub fn pop_back(&mut self) -> Option<T> {
        self.raw.pop_back()
    }

    #[inline]
    pub fn pop_front(&mut self) -> Option<T> {
        self.raw.pop_front()
    }

    /// Insert an element at `index`, pushing any elements with indices greater than or equal to
    /// `index` toward the back.
    #[inline]
    pub fn insert_at(&mut self, index: I, value: T) {
        self.raw.insert(index.index(), value)
    }

    #[inline]
    pub fn iter(&self) -> vec_deque::Iter<'_, T> {
        self.raw.iter()
    }

    #[inline]
    pub fn iter_mut(&mut self) -> vec_deque::IterMut<'_, T> {
        self.raw.iter_mut()
    }

    #[inline]
    pub fn iter_enumerated(&self) -> impl DoubleEndedIterator<Item = (I, &T)> + ExactSizeIterator {
        // Allow the optimizer to elide the bounds checking when creating each index.
        let _ = I::new(self.len());
        self.raw.iter().enumerate().map(|(n, t)| (I::new(n), t))
    }

    #[inline]
    pub fn iter_enumerated_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = (I, &mut T)> + ExactSizeIterator {
        // Allow the optimizer to elide the bounds checking when creating each index.
        let _ = I::new(self.len());
        self.raw.iter_mut().enumerate().map(|(n, t)| (I::new(n), t))
    }

    #[inline]
    pub fn into_iter_inner(self) -> vec_deque::IntoIter<T> {
        self.raw.into_iter()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn next_index(&self) -> I {
        I::new(self.raw.len())
    }

    #[inline]
    pub fn last_index(&self) -> Option<I> {
        self.len().checked_sub(1).map(I::new)
    }

    #[inline]
    pub fn get(&self, index: I) -> Option<&T> {
        self.raw.get(index.index())
    }

    #[inline]
    pub fn get_mut(&mut self, index: I) -> Option<&mut T> {
        self.raw.get_mut(index.index())
    }

    #[inline]
    pub fn into_iter_enumerated(
        self,
    ) -> impl DoubleEndedIterator<Item = (I, T)> + ExactSizeIterator {
        // Allow the optimizer to elide the bounds checking when creating each index.
        let _ = I::new(self.len());
        self.raw
            .into_iter()
            .enumerate()
            .map(|(n, t)| (I::new(n), t))
    }

    #[inline]
    pub fn drain<R: RangeBounds<usize>>(&mut self, range: R) -> impl Iterator<Item = T> {
        self.raw.drain(range)
    }

    #[inline]
    pub fn drain_enumerated<R: RangeBounds<usize>>(
        &mut self,
        range: R,
    ) -> impl Iterator<Item = (I, T)> {
        let begin = match range.start_bound() {
            std::ops::Bound::Included(i) => *i,
            std::ops::Bound::Excluded(i) => i.checked_add(1).unwrap(),
            std::ops::Bound::Unbounded => 0,
        };
        self.raw
            .drain(range)
            .enumerate()
            .map(move |(n, t)| (I::new(begin + n), t))
    }

    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.raw.shrink_to_fit()
    }

    #[inline]
    pub fn truncate(&mut self, a: usize) {
        self.raw.truncate(a)
    }

    /// Grows the index vector so that it contains an entry for
    /// `elem`; if that is already true, then has no
    /// effect. Otherwise, inserts new values as needed by invoking
    /// `fill_value`.
    ///
    /// Returns a reference to the `elem` entry.
    #[inline]
    pub fn ensure_contains_elem(&mut self, elem: I, fill_value: impl FnMut() -> T) -> &mut T {
        let min_new_len = elem.index() + 1;
        if self.len() < min_new_len {
            self.raw.resize_with(min_new_len, fill_value);
        }

        &mut self[elem]
    }

    #[inline]
    pub fn resize(&mut self, new_len: usize, value: T)
    where
        T: Clone,
    {
        self.raw.resize(new_len, value)
    }

    #[inline]
    pub fn resize_to_elem(&mut self, elem: I, fill_value: impl FnMut() -> T) {
        let min_new_len = elem.index() + 1;
        self.raw.resize_with(min_new_len, fill_value);
    }

    #[inline]
    pub fn append(&mut self, other: &mut Self) {
        self.raw.append(&mut other.raw);
    }
}

impl<I: Idx, T> std::ops::Index<I> for IndexVecDeque<I, T> {
    type Output = T;
    #[inline]
    fn index(&self, index: I) -> &T {
        &self.raw[index.index()]
    }
}

impl<I: Idx, T> std::ops::IndexMut<I> for IndexVecDeque<I, T> {
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut T {
        &mut self.raw[index.index()]
    }
}

/// `IndexVec` is often used as a map, so it provides some map-like APIs.
impl<I: Idx, T> IndexVecDeque<I, Option<T>> {
    #[inline]
    pub fn insert(&mut self, index: I, value: T) -> Option<T> {
        self.ensure_contains_elem(index, || None).replace(value)
    }

    #[inline]
    pub fn get_or_insert_with(&mut self, index: I, value: impl FnOnce() -> T) -> &mut T {
        self.ensure_contains_elem(index, || None)
            .get_or_insert_with(value)
    }

    #[inline]
    pub fn remove(&mut self, index: I) -> Option<T> {
        self.get_mut(index)?.take()
    }

    #[inline]
    pub fn contains(&self, index: I) -> bool {
        self.get(index).and_then(Option::as_ref).is_some()
    }
}

impl<I: Idx, T: fmt::Debug> fmt::Debug for IndexVecDeque<I, T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.raw, fmt)
    }
}

impl<I: Idx, T> Extend<T> for IndexVecDeque<I, T> {
    #[inline]
    fn extend<J: IntoIterator<Item = T>>(&mut self, iter: J) {
        self.raw.extend(iter);
    }
}

impl<I: Idx, T> FromIterator<T> for IndexVecDeque<I, T> {
    #[inline]
    fn from_iter<J>(iter: J) -> Self
    where
        J: IntoIterator<Item = T>,
    {
        IndexVecDeque::from_raw(VecDeque::from_iter(iter))
    }
}

impl<I: Idx, T> IntoIterator for IndexVecDeque<I, T> {
    type Item = T;
    type IntoIter = vec_deque::IntoIter<T>;

    #[inline]
    fn into_iter(self) -> vec_deque::IntoIter<T> {
        self.raw.into_iter()
    }
}

impl<'a, I: Idx, T> IntoIterator for &'a IndexVecDeque<I, T> {
    type Item = &'a T;
    type IntoIter = vec_deque::Iter<'a, T>;

    #[inline]
    fn into_iter(self) -> vec_deque::Iter<'a, T> {
        self.raw.iter()
    }
}

impl<'a, I: Idx, T> IntoIterator for &'a mut IndexVecDeque<I, T> {
    type Item = &'a mut T;
    type IntoIter = vec_deque::IterMut<'a, T>;

    #[inline]
    fn into_iter(self) -> vec_deque::IterMut<'a, T> {
        self.raw.iter_mut()
    }
}

impl<I: Idx, T> Default for IndexVecDeque<I, T> {
    #[inline]
    fn default() -> Self {
        IndexVecDeque::new()
    }
}

impl<I: Idx, T, const N: usize> From<[T; N]> for IndexVecDeque<I, T> {
    #[inline]
    fn from(array: [T; N]) -> Self {
        IndexVecDeque::from_raw(array.into())
    }
}

// SAFETY: `Send` is an unsafe trait. Whether `IndexVec` is `Send` depends only
// on the data, not the phantom data.
unsafe impl<I: Idx, T> Send for IndexVecDeque<I, T> where T: Send {}
