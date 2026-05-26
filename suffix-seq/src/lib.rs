use parking_lot::RwLock;
use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Internal node of the SuffixSeq.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    Nil,
    Cons(T, SuffixSeq<T>, usize),
}

#[cfg(feature = "serde")]
impl<T> Serialize for Node<T>
where
    T: Serialize + Hash + Eq + Clone + Send + Sync + 'static,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Node::Nil => serializer.serialize_unit_variant("Node", 0, "Nil"),
            Node::Cons(head, tail, _) => {
                use serde::ser::SerializeStruct;
                let mut state = serializer.serialize_struct("Node", 2)?;
                state.serialize_field("head", head)?;
                state.serialize_field("tail", tail)?;
                state.end()
            }
        }
    }
}

#[cfg(feature = "serde")]
impl<'de, T> Deserialize<'de> for Node<T>
where
    T: Deserialize<'de> + Hash + Eq + Clone + Send + Sync + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum NodeData<T>
        where
            T: Hash + Eq + Clone + Send + Sync + 'static,
        {
            Nil,
            Cons { head: T, tail: SuffixSeq<T> },
        }
        let data = NodeData::deserialize(deserializer)?;
        match data {
            NodeData::Nil => Ok(Node::Nil),
            NodeData::Cons { head, tail } => {
                let len = tail.len() + 1;
                Ok(Node::Cons(head, tail, len))
            }
        }
    }
}

struct SuffixInterner<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    shards: Box<[RwLock<HashSet<&'static Node<T>>>]>,
}

impl<T> SuffixInterner<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(RwLock::new(HashSet::new()));
        }
        Self {
            shards: shards.into_boxed_slice(),
        }
    }

    fn shard_for(&self, node: &Node<T>) -> usize {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        node.hash(&mut s);
        let hash = s.finish();
        (hash % self.shards.len() as u64) as usize
    }

    fn intern(&self, node: &Node<T>) -> &'static Node<T> {
        let idx = self.shard_for(node);
        let shard = &self.shards[idx];

        {
            let read = shard.read();
            if let Some(&existing) = read.get(node) {
                return existing;
            }
        }

        let mut write = shard.write();
        if let Some(&existing) = write.get(node) {
            return existing;
        }

        let leaked: &'static Node<T> = Box::leak(Box::new(node.clone()));
        write.insert(leaked);
        leaked
    }
}

static INTERNERS: OnceLock<RwLock<HashMap<TypeId, &'static (dyn Any + Send + Sync)>>> =
    OnceLock::new();

fn get_interner<T>() -> &'static SuffixInterner<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    let type_id = TypeId::of::<T>();
    let map_lock = INTERNERS.get_or_init(|| RwLock::new(HashMap::new()));

    if let Some(&interner) = map_lock.read().get(&type_id) {
        return interner
            .downcast_ref::<SuffixInterner<T>>()
            .expect("Type mismatch in interner registry");
    }

    let mut map = map_lock.write();
    let interner = map.entry(type_id).or_insert_with(|| {
        let interner = SuffixInterner::<T>::new(64);
        let leaked: &'static SuffixInterner<T> = Box::leak(Box::new(interner));
        leaked as &'static (dyn Any + Send + Sync)
    });

    interner
        .downcast_ref::<SuffixInterner<T>>()
        .expect("Type mismatch in interner registry")
}

/// A suffix-compressed sequence of elements of type `T`.
/// Interned for memory efficiency and fast equality checks.
#[derive(Debug)]
pub struct SuffixSeq<T>(&'static Node<T>)
where
    T: Hash + Eq + Clone + Send + Sync + 'static;

impl<T> SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    pub fn intern(node: Node<T>) -> Self {
        SuffixSeq(get_interner::<T>().intern(&node))
    }

    /// Creates an empty `SuffixSeq`.
    pub fn new() -> Self {
        Self::intern(Node::Nil)
    }

    /// Returns a new `SuffixSeq` with `item` prepended to the current sequence.
    pub fn push_front(&self, item: T) -> Self {
        let len = self.len() + 1;
        Self::intern(Node::Cons(item, *self, len))
    }

    /// Returns the head element of the sequence, if it's not empty.
    pub fn head(&self) -> Option<&T> {
        match self.0 {
            Node::Nil => None,
            Node::Cons(head, _, _) => Some(head),
        }
    }

    /// Returns the tail of the sequence, if it's not empty.
    pub fn tail(&self) -> Option<Self> {
        match self.0 {
            Node::Nil => None,
            Node::Cons(_, tail, _) => Some(*tail),
        }
    }

    /// Returns `true` if the sequence is empty.
    pub fn is_empty(&self) -> bool {
        matches!(self.0, Node::Nil)
    }

    /// Returns the length of the sequence.
    pub fn len(&self) -> usize {
        match self.0 {
            Node::Nil => 0,
            Node::Cons(_, _, len) => *len,
        }
    }

    /// Returns a new `SuffixSeq` with `item` appended to the current sequence.
    /// $O(n)$ complexity as it requires rebuilding the sequence.
    pub fn push_back(&self, item: T) -> Self {
        let mut items: Vec<_> = self.iter().cloned().collect();
        items.push(item);
        items.into_iter().collect()
    }

    /// Returns the last element of the sequence, if it's not empty.
    /// $O(n)$ complexity.
    pub fn last(&self) -> Option<&'static T> {
        self.iter().last()
    }

    /// Returns a new `SuffixSeq` containing all but the last element of the current sequence.
    /// $O(n)$ complexity.
    pub fn all_but_last(&self) -> Option<Self> {
        if self.is_empty() {
            return None;
        }
        let items: Vec<_> = self.iter().cloned().collect();
        if items.is_empty() {
            return Some(Self::new());
        }
        Some(items[..items.len() - 1].iter().cloned().collect())
    }

    /// Returns a new `SuffixSeq` that is the concatenation of `self` and `other`.
    /// Shares `other`'s backing memory. $O(self.len())$ complexity.
    pub fn concat(&self, other: &Self) -> Self {
        let items: Vec<_> = self.iter().cloned().collect();
        let mut result = *other;
        for item in items.into_iter().rev() {
            result = result.push_front(item);
        }
        result
    }

    /// Returns the suffix of the sequence starting at index `n`.
    /// If `n >= len()`, returns an empty sequence.
    pub fn suffix(&self, n: usize) -> Self {
        let mut current = *self;
        for _ in 0..n {
            if let Some(tail) = current.tail() {
                current = tail;
            } else {
                return Self::new();
            }
        }
        current
    }

    /// Returns an iterator over the elements of the sequence.
    pub fn iter(&self) -> Iter<T> {
        Iter {
            current: Some(*self),
        }
    }
}

impl<T> Clone for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for SuffixSeq<T> where T: Hash + Eq + Clone + Send + Sync + 'static {}

impl<T> PartialEq for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl<T> Eq for SuffixSeq<T> where T: Hash + Eq + Clone + Send + Sync + 'static {}

impl<T> PartialOrd for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static + PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self == other {
            return Some(std::cmp::Ordering::Equal);
        }
        self.iter().partial_cmp(other.iter())
    }
}

impl<T> Ord for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static + Ord,
{
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self == other {
            return std::cmp::Ordering::Equal;
        }
        self.iter().cmp(other.iter())
    }
}

impl<T> Hash for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

#[cfg(feature = "serde")]
impl<T> Serialize for SuffixSeq<T>
where
    T: Serialize + Hash + Eq + Clone + Send + Sync + 'static,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, T> Deserialize<'de> for SuffixSeq<T>
where
    T: Deserialize<'de> + Hash + Eq + Clone + Send + Sync + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let node = Node::deserialize(deserializer)?;
        Ok(Self::intern(node))
    }
}

impl<T> Default for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// An iterator over the elements of a `SuffixSeq`.
pub struct Iter<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    current: Option<SuffixSeq<T>>,
}

impl<T> Iterator for Iter<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    type Item = &'static T;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.current?;
        match current.0 {
            Node::Nil => {
                self.current = None;
                None
            }
            Node::Cons(head, tail, _) => {
                self.current = Some(*tail);
                Some(head)
            }
        }
    }
}

impl<T> FromIterator<T> for SuffixSeq<T>
where
    T: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let items: Vec<T> = iter.into_iter().collect();
        let mut seq = SuffixSeq::new();
        for item in items.into_iter().rev() {
            seq = seq.push_front(item);
        }
        seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_front_and_iter() {
        let seq = SuffixSeq::new().push_front(3).push_front(2).push_front(1);
        let items: Vec<_> = seq.iter().cloned().collect();
        assert_eq!(items, vec![1, 2, 3]);
    }

    #[test]
    fn test_sharing() {
        let suffix = SuffixSeq::new().push_front('c').push_front('b');
        let seq1 = suffix.push_front('a');
        let seq2 = suffix.push_front('x');

        assert_eq!(seq1.tail().unwrap(), seq2.tail().unwrap());
        assert!(std::ptr::eq(seq1.tail().unwrap().0, seq2.tail().unwrap().0));
    }

    #[test]
    fn test_from_iter() {
        let seq: SuffixSeq<i32> = vec![1, 2, 3].into_iter().collect();
        let items: Vec<_> = seq.iter().cloned().collect();
        assert_eq!(items, vec![1, 2, 3]);
    }

    #[test]
    fn test_abc_example() {
        // .a.b.c
        let c = SuffixSeq::new().push_front('c');
        let b_c = c.push_front('b');
        let a_b_c = b_c.push_front('a');

        let items: Vec<_> = a_b_c.iter().cloned().collect();
        assert_eq!(items, vec!['a', 'b', 'c']);

        // Check sharing
        let x_b_c = b_c.push_front('x');
        assert!(std::ptr::eq(
            a_b_c.tail().unwrap().0,
            x_b_c.tail().unwrap().0
        ));
    }

    #[test]
    fn test_deep_sharing() {
        let mut seq1 = SuffixSeq::new();
        for i in (0..100).rev() {
            seq1 = seq1.push_front(i);
        }

        let mut seq2 = SuffixSeq::new();
        for i in (0..100).rev() {
            seq2 = seq2.push_front(i);
        }

        assert_eq!(seq1, seq2);
        assert!(std::ptr::eq(seq1.0, seq2.0));
    }
}
