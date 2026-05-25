use parking_lot::RwLock;
use std::any::{Any, TypeId};
use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Internal node of the Trie.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Node<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    children: BTreeMap<T, Trie<T, V>>,
    value: Option<V>,
}

#[cfg(feature = "serde")]
impl<T, V> Serialize for Node<T, V>
where
    T: Serialize + Ord + Hash + Clone + Send + Sync + 'static,
    V: Serialize + Eq + Hash + Clone + Send + Sync + 'static,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Node", 2)?;
        state.serialize_field("children", &self.children)?;
        state.serialize_field("value", &self.value)?;
        state.end()
    }
}

#[cfg(feature = "serde")]
impl<'de, T, V> Deserialize<'de> for Node<T, V>
where
    T: Deserialize<'de> + Ord + Hash + Clone + Send + Sync + 'static,
    V: Deserialize<'de> + Eq + Hash + Clone + Send + Sync + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct NodeData<T, V>
        where
            T: Ord + Hash + Clone + Send + Sync + 'static,
            V: Eq + Hash + Clone + Send + Sync + 'static,
        {
            children: BTreeMap<T, Trie<T, V>>,
            value: Option<V>,
        }
        let data = NodeData::deserialize(deserializer)?;
        Ok(Node {
            children: data.children,
            value: data.value,
        })
    }
}

struct TrieInterner<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    shards: Box<[RwLock<HashSet<&'static Node<T, V>>>]>,
}

impl<T, V> TrieInterner<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
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

    fn shard_for(&self, node: &Node<T, V>) -> usize {
        let mut s = std::collections::hash_map::DefaultHasher::new();
        node.hash(&mut s);
        let hash = s.finish();
        (hash % self.shards.len() as u64) as usize
    }

    fn intern(&self, node: &Node<T, V>) -> &'static Node<T, V> {
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

        let leaked: &'static Node<T, V> = Box::leak(Box::new(node.clone()));
        write.insert(leaked);
        leaked
    }
}

static INTERNERS: OnceLock<RwLock<HashMap<TypeId, &'static (dyn Any + Send + Sync)>>> =
    OnceLock::new();

fn get_interner<T, V>() -> &'static TrieInterner<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    let type_id = TypeId::of::<(T, V)>();
    let map_lock = INTERNERS.get_or_init(|| RwLock::new(HashMap::new()));

    if let Some(&interner) = map_lock.read().get(&type_id) {
        return interner
            .downcast_ref::<TrieInterner<T, V>>()
            .expect("Type mismatch in interner registry");
    }

    let mut map = map_lock.write();
    let interner = map.entry(type_id).or_insert_with(|| {
        let interner = TrieInterner::<T, V>::new(64);
        let leaked: &'static TrieInterner<T, V> = Box::leak(Box::new(interner));
        leaked as &'static (dyn Any + Send + Sync)
    });

    interner
        .downcast_ref::<TrieInterner<T, V>>()
        .expect("Type mismatch in interner registry")
}

/// A prefix tree (Trie) that stores sequences of type `T` mapped to values of type `V`.
/// Interned for memory efficiency and fast equality checks.
#[derive(Debug)]
pub struct Trie<T, V>(&'static Node<T, V>)
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static;

impl<T, V> Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn intern(node: Node<T, V>) -> Self {
        Trie(get_interner::<T, V>().intern(&node))
    }
}

impl<T, V> Clone for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, V> Copy for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
}

impl<T, V> PartialEq for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl<T, V> Eq for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
}

impl<T, V> PartialOrd for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self == other {
            return Some(std::cmp::Ordering::Equal);
        }
        
        if self.is_subset(other) {
            Some(std::cmp::Ordering::Less)
        } else if self.is_superset(other) {
            Some(std::cmp::Ordering::Greater)
        } else {
            None
        }
    }
}

impl<T, V> Hash for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

#[cfg(feature = "serde")]
impl<T, V> Serialize for Trie<T, V>
where
    T: Serialize + Ord + Hash + Clone + Send + Sync + 'static,
    V: Serialize + Eq + Hash + Clone + Send + Sync + 'static,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, T, V> Deserialize<'de> for Trie<T, V>
where
    T: Deserialize<'de> + Ord + Hash + Clone + Send + Sync + 'static,
    V: Deserialize<'de> + Eq + Hash + Clone + Send + Sync + 'static,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let node = Node::deserialize(deserializer)?;
        Ok(Self::intern(node))
    }
}

impl<T, V> Default for Trie<T, V>
where
    T: Ord + Hash + Clone + Send + Sync + 'static,
    V: Eq + Hash + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord + Hash + Clone + Send + Sync + 'static, V: Eq + Hash + Clone + Send + Sync + 'static>
    Trie<T, V>
{
    /// Creates a new, empty `Trie`.
    pub fn new() -> Self {
        Self::intern(Node {
            children: BTreeMap::new(),
            value: None,
        })
    }

    /// Returns `true` if the `Trie` contains no sequences.
    pub fn is_empty(&self) -> bool {
        self.0.children.is_empty() && self.0.value.is_none()
    }

    /// Returns the number of sequences in the `Trie`.
    pub fn len(&self) -> usize {
        let mut count = if self.0.value.is_some() { 1 } else { 0 };
        for child in self.0.children.values() {
            count += child.len();
        }
        count
    }

    /// Inserts a sequence into the `Trie` with a value.
    pub fn insert<I>(&mut self, sequence: I, value: V)
    where
        I: IntoIterator<Item = T>,
    {
        let mut sequence = sequence.into_iter();
        let mut node = (*self.0).clone();
        if let Some(item) = sequence.next() {
            let mut child = node.children.get(&item).cloned().unwrap_or_default();
            child.insert(sequence, value);
            node.children.insert(item, child);
        } else {
            node.value = Some(value);
        }
        *self = Self::intern(node);
    }

    /// Returns the value associated with the given sequence, if it exists.
    pub fn get<I>(&self, sequence: I) -> Option<&V>
    where
        I: IntoIterator,
        I::Item: Borrow<T>,
    {
        let mut current = self;
        for item in sequence {
            if let Some(next) = current.0.children.get(item.borrow()) {
                current = next;
            } else {
                return None;
            }
        }
        current.0.value.as_ref()
    }

    /// Returns `true` if the `Trie` contains the given sequence.
    pub fn contains_key<I>(&self, sequence: I) -> bool
    where
        I: IntoIterator,
        I::Item: Borrow<T>,
    {
        self.get(sequence).is_some()
    }

    /// Returns `true` if the `Trie` contains a prefix of the given sequence.
    pub fn contains_prefix<I>(&self, sequence: I) -> bool
    where
        I: IntoIterator,
        I::Item: Borrow<T>,
    {
        let mut current = self;
        for item in sequence {
            if let Some(next) = current.0.children.get(item.borrow()) {
                current = next;
            } else {
                return false;
            }
        }
        true
    }

    /// Returns the sub-trie at the given prefix, if it exists.
    pub fn get_subtrie<I>(&self, sequence: I) -> Option<Self>
    where
        I: IntoIterator,
        I::Item: Borrow<T>,
    {
        let mut current = self;
        for item in sequence {
            if let Some(next) = current.0.children.get(item.borrow()) {
                current = next;
            } else {
                return None;
            }
        }
        Some(current.clone())
    }

    /// Removes all sequences starting with the given prefix.
    /// Returns `true` if the trie was modified.
    pub fn remove_prefix<I>(&mut self, prefix: I) -> bool
    where
        I: IntoIterator,
        I::Item: Borrow<T>,
    {
        fn remove_recursive<
            T: Ord + Hash + Clone + Send + Sync + 'static,
            V: Eq + Hash + Clone + Send + Sync + 'static,
            I,
        >(
            trie: &mut Trie<T, V>,
            mut prefix: I,
        ) -> bool
        where
            I: Iterator,
            I::Item: Borrow<T>,
        {
            if let Some(item) = prefix.next() {
                let item_borrow = item.borrow();
                let mut node = (*trie.0).clone();
                if let Some(mut child) = node.children.get(item_borrow).cloned() {
                    if remove_recursive(&mut child, prefix) {
                        if child.0.children.is_empty() && child.0.value.is_none() {
                            node.children.remove(item_borrow);
                        } else {
                            node.children.insert(item_borrow.clone(), child);
                        }
                        *trie = Trie::intern(node);
                        return true;
                    }
                }
                false
            } else {
                let empty = Trie::new();
                if *trie != empty {
                    *trie = empty;
                    true
                } else {
                    false
                }
            }
        }

        remove_recursive(self, prefix.into_iter())
    }

    /// Returns a new trie with the given prefix replaced by a new prefix.
    /// If the prefix does not match any sequences, None is returned.
    pub fn substitute_prefix<I, J>(&self, prefix: I, new_prefix: J) -> Option<Self>
    where
        I: IntoIterator + Clone,
        I::Item: Borrow<T>,
        J: IntoIterator<Item = T>,
    {
        if let Some(subtrie) = self.get_subtrie(prefix.clone()) {
            let mut result = self.clone();
            result.remove_prefix(prefix);

            let mut substituted = subtrie;
            let mut items: Vec<T> = new_prefix.into_iter().collect();
            while let Some(item) = items.pop() {
                let mut node = Node {
                    children: BTreeMap::new(),
                    value: None,
                };
                node.children.insert(item, substituted);
                substituted = Self::intern(node);
            }

            result.union(&substituted);
            Some(result)
        } else {
            None
        }
    }

    /// Merges another `Trie` into this one.
    /// If both tries have a value for the same sequence, the value from `other` is used.
    pub fn union(&mut self, other: &Self) {
        if self == other {
            return;
        }

        let mut node = (*self.0).clone();
        let other_node = &*other.0;

        let mut modified = false;
        if let Some(other_val) = &other_node.value {
            if node.value.as_ref() != Some(other_val) {
                node.value = Some(other_val.clone());
                modified = true;
            }
        }

        for (item, other_child) in &other_node.children {
            if let Some(child) = node.children.get_mut(item) {
                let old_child_ptr = child.0 as *const _;
                child.union(other_child);
                if !std::ptr::eq(old_child_ptr, child.0 as *const _) {
                    modified = true;
                }
            } else {
                node.children.insert(item.clone(), other_child.clone());
                modified = true;
            }
        }

        if modified {
            *self = Self::intern(node);
        }
    }

    /// Returns a new `Trie` containing only the sequences that satisfy the predicate.
    ///
    /// # Example
    ///
    /// ```
    /// use trie::Trie;
    /// let mut trie = Trie::new();
    /// trie.insert("apple".chars(), 1);
    /// trie.insert("banana".chars(), 2);
    ///
    /// let filtered = trie.filter(|seq, _| seq.starts_with(&['a']));
    /// assert_eq!(filtered.len(), 1);
    /// assert!(filtered.contains_key("apple".chars()));
    /// ```
    pub fn filter<F>(&self, mut predicate: F) -> Self
    where
        F: FnMut(&[T], &V) -> bool,
    {
        fn filter_recursive<
            T: Ord + Hash + Clone + Send + Sync + 'static,
            V: Eq + Hash + Clone + Send + Sync + 'static,
            F: FnMut(&[T], &V) -> bool,
        >(
            trie: &Trie<T, V>,
            current: &mut Vec<T>,
            predicate: &mut F,
        ) -> Trie<T, V> {
            let mut new_value = None;
            if let Some(val) = &trie.0.value {
                if predicate(current, val) {
                    new_value = Some(val.clone());
                }
            }

            let mut new_children = BTreeMap::new();
            for (item, child) in &trie.0.children {
                current.push(item.clone());
                let filtered_child = filter_recursive(child, current, predicate);
                current.pop();

                if !filtered_child.is_empty() {
                    new_children.insert(item.clone(), filtered_child);
                }
            }

            Trie::intern(Node {
                children: new_children,
                value: new_value,
            })
        }

        filter_recursive(self, &mut Vec::new(), &mut predicate)
    }

    /// Returns the children of the root node.
    pub fn children(&self) -> &BTreeMap<T, Trie<T, V>> {
        &self.0.children
    }

    /// Returns `true` if the root node is a terminal node.
    pub fn is_terminal(&self) -> bool {
        self.0.value.is_some()
    }

    /// Returns the value at the root node, if it exists.
    pub fn value(&self) -> Option<&V> {
        self.0.value.as_ref()
    }

    /// Returns all sequences and their values in the `Trie`.
    pub fn all_sequences(&self) -> Vec<(Vec<T>, V)> {
        let mut result = Vec::new();
        fn collect<
            T: Ord + Hash + Clone + Send + Sync + 'static,
            V: Eq + Hash + Clone + Send + Sync + 'static,
        >(
            trie: &Trie<T, V>,
            current: &mut Vec<T>,
            result: &mut Vec<(Vec<T>, V)>,
        ) {
            if let Some(val) = trie.value() {
                result.push((current.clone(), val.clone()));
            }
            for (item, child) in trie.children() {
                current.push(item.clone());
                collect(child, current, result);
                current.pop();
            }
        }
        collect(self, &mut Vec::new(), &mut result);
        result
    }

    /// Returns `true` if this trie is a subset of another trie.
    /// A trie is a subset of another if all its sequences are present in the other trie
    /// and their associated values are equal.
    pub fn is_subset(&self, other: &Self) -> bool {
        if self == other {
            return true;
        }

        if let Some(val) = &self.0.value {
            if other.0.value.as_ref() != Some(val) {
                return false;
            }
        }

        for (item, child) in &self.0.children {
            if let Some(other_child) = other.0.children.get(item) {
                if !child.is_subset(other_child) {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }

    /// Returns `true` if this trie is a superset of another trie.
    pub fn is_superset(&self, other: &Self) -> bool {
        other.is_subset(self)
    }

    /// Keeps only the sequences that are present in both `Tries`.
    /// If both tries have a value for the same sequence, the value from `self` is kept.
    pub fn intersection(&mut self, other: &Self) {
        if self == other {
            return;
        }

        let mut node = (*self.0).clone();
        let other_node = &*other.0;

        let mut modified = false;
        if node.value.is_some() && other_node.value.is_none() {
            node.value = None;
            modified = true;
        }

        let mut keys_to_remove = Vec::new();
        for (item, child) in &mut node.children {
            if let Some(other_child) = other_node.children.get(item) {
                let old_child_ptr = child.0 as *const _;
                child.intersection(other_child);
                if !std::ptr::eq(old_child_ptr, child.0 as *const _) {
                    modified = true;
                }
                if child.0.value.is_none() && child.0.children.is_empty() {
                    keys_to_remove.push(item.clone());
                    modified = true;
                }
            } else {
                keys_to_remove.push(item.clone());
                modified = true;
            }
        }

        for key in keys_to_remove {
            node.children.remove(&key);
        }

        if modified {
            *self = Self::intern(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_contains() {
        let mut trie = Trie::new();
        trie.insert(vec![1, 2, 3], "val1");
        assert!(trie.contains_key(&[1, 2, 3]));
        assert_eq!(trie.get(&[1, 2, 3]), Some(&"val1"));
        assert!(!trie.contains_key(&[1, 2]));
        assert!(!trie.contains_key(&[1, 2, 3, 4]));

        trie.insert(vec![1, 2], "val2");
        assert!(trie.contains_key(&[1, 2]));
        assert_eq!(trie.get(&[1, 2]), Some(&"val2"));
    }

    #[test]
    fn test_contains_prefix() {
        let mut trie = Trie::new();
        trie.insert(vec!['a', 'b', 'c'], 1);
        assert!(trie.contains_prefix(&['a', 'b']));
        assert!(trie.contains_prefix(&['a', 'b', 'c']));
        assert!(!trie.contains_prefix(&['a', 'd']));
    }

    #[test]
    fn test_empty_trie() {
        let trie: Trie<i32, i32> = Trie::new();
        assert!(trie.is_empty());
        assert_eq!(trie.len(), 0);
        assert!(!trie.contains_key::<Vec<i32>>(vec![]));
        assert!(trie.contains_prefix::<Vec<i32>>(vec![]));
    }

    #[test]
    fn test_len_and_is_empty() {
        let mut trie = Trie::new();
        assert!(trie.is_empty());
        assert_eq!(trie.len(), 0);

        trie.insert(vec![1, 2, 3], "a");
        assert!(!trie.is_empty());
        assert_eq!(trie.len(), 1);

        trie.insert(vec![1, 2], "b");
        assert_eq!(trie.len(), 2);

        trie.insert(vec![1, 2, 3], "c"); // update value
        assert_eq!(trie.len(), 2);
        assert_eq!(trie.get(&[1, 2, 3]), Some(&"c"));

        let mut t2 = Trie::new();
        t2.insert(vec![4, 5], "d");
        trie.union(&t2);
        assert_eq!(trie.len(), 3);
    }

    #[test]
    fn test_insert_empty() {
        let mut trie = Trie::new();
        trie.insert(Vec::<i32>::new(), "empty");
        assert!(trie.contains_key::<Vec<i32>>(vec![]));
        assert_eq!(trie.get::<Vec<i32>>(vec![]), Some(&"empty"));
    }

    #[test]
    fn test_multiple_sequences() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);
        trie.insert("app".chars(), 2);
        trie.insert("apply".chars(), 3);
        trie.insert("bat".chars(), 4);

        assert_eq!(trie.get("apple".chars()), Some(&1));
        assert_eq!(trie.get("app".chars()), Some(&2));
        assert_eq!(trie.get("apply".chars()), Some(&3));
        assert_eq!(trie.get("bat".chars()), Some(&4));

        assert!(!trie.contains_key("ap".chars()));
        assert!(!trie.contains_key("apples".chars()));
        assert!(!trie.contains_key("ba".chars()));
        assert!(!trie.contains_key("bath".chars()));
    }

    #[test]
    fn test_contains_prefix_complex() {
        let mut trie = Trie::new();
        trie.insert("hello".chars(), ());

        assert!(trie.contains_prefix("h".chars()));
        assert!(trie.contains_prefix("he".chars()));
        assert!(trie.contains_prefix("hel".chars()));
        assert!(trie.contains_prefix("hell".chars()));
        assert!(trie.contains_prefix("hello".chars()));
        assert!(!trie.contains_prefix("hellos".chars()));
        assert!(!trie.contains_prefix("world".chars()));
    }

    #[test]
    fn test_interning() {
        let mut trie = Trie::new();
        trie.insert("abc".chars(), 1);
        trie.insert("dbc".chars(), 1);

        // The sub-trie for "bc" should be interned and shared.
        let child_a = trie.0.children.get(&'a').unwrap();
        let child_d = trie.0.children.get(&'d').unwrap();

        let sub_bc_a = child_a.0.children.get(&'b').unwrap();
        let sub_bc_d = child_d.0.children.get(&'b').unwrap();

        assert_eq!(sub_bc_a, sub_bc_d);
        assert!(std::ptr::eq(sub_bc_a.0, sub_bc_d.0));
    }

    #[test]
    fn test_idempotent_insert() {
        let mut trie = Trie::new();
        trie.insert("abc".chars(), 1);
        let original_ptr = trie.0 as *const _;

        trie.insert("abc".chars(), 1);
        let new_ptr = trie.0 as *const _;

        assert_eq!(original_ptr, new_ptr);
    }

    #[test]
    fn test_union() {
        let mut t1 = Trie::new();
        t1.insert("apple".chars(), 1);
        t1.insert("banana".chars(), 2);

        let mut t2 = Trie::new();
        t2.insert("apple".chars(), 10);
        t2.insert("cherry".chars(), 3);

        t1.union(&t2);

        assert_eq!(t1.get("apple".chars()), Some(&10)); // Overwritten by other
        assert_eq!(t1.get("banana".chars()), Some(&2));
        assert_eq!(t1.get("cherry".chars()), Some(&3));
    }

    #[test]
    fn test_intersection() {
        let mut t1 = Trie::new();
        t1.insert("apple".chars(), 1);
        t1.insert("banana".chars(), 2);
        t1.insert("app".chars(), 3);

        let mut t2 = Trie::new();
        t2.insert("apple".chars(), 10);
        t2.insert("cherry".chars(), 4);
        t2.insert("ap".chars(), 5);

        t1.intersection(&t2);

        assert_eq!(t1.get("apple".chars()), Some(&1)); // Kept from self
        assert!(!t1.contains_key("banana".chars()));
        assert!(!t1.contains_key("cherry".chars()));
        assert!(!t1.contains_key("app".chars()));
        assert!(!t1.contains_key("ap".chars()));
    }

    #[test]
    fn test_union_empty() {
        let mut t1 = Trie::new();
        t1.insert("abc".chars(), 1);
        let t2 = Trie::new();

        let original_ptr = t1.0 as *const _;
        t1.union(&t2);
        assert_eq!(original_ptr, t1.0 as *const _);
        assert!(t1.contains_key("abc".chars()));

        let mut t3 = Trie::new();
        t3.union(&t1);
        assert!(t3.contains_key("abc".chars()));
    }

    #[test]
    fn test_intersection_empty() {
        let mut t1 = Trie::new();
        t1.insert("abc".chars(), 1);
        let t2 = Trie::new();

        t1.intersection(&t2);
        assert!(!t1.contains_key("abc".chars()));
        assert!(t1.0.children.is_empty());
        assert!(t1.0.value.is_none());
    }

    #[test]
    fn test_intersection_nested() {
        let mut t1 = Trie::new();
        t1.insert(vec![1, 2, 3], 1);
        t1.insert(vec![1, 2, 4], 2);

        let mut t2 = Trie::new();
        t2.insert(vec![1, 2, 3], 10);
        t2.insert(vec![1, 5], 3);

        t1.intersection(&t2);

        assert!(t1.contains_key(vec![1, 2, 3]));
        assert!(!t1.contains_key(vec![1, 2, 4]));
        assert!(!t1.contains_key(vec![1, 5]));

        // Ensure it doesn't contain [1, 2] if it wasn't terminal
        assert!(!t1.contains_key(vec![1, 2]));
        assert!(t1.contains_prefix(vec![1, 2]));
    }

    #[test]
    fn test_get_subtrie() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);
        trie.insert("apply".chars(), 2);

        let sub = trie.get_subtrie("app".chars()).unwrap();
        assert_eq!(sub.get("le".chars()), Some(&1));
        assert_eq!(sub.get("ly".chars()), Some(&2));
        assert!(!sub.contains_key("apple".chars()));
    }

    #[test]
    fn test_remove_prefix() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);
        trie.insert("apply".chars(), 2);
        trie.insert("banana".chars(), 3);

        trie.remove_prefix("app".chars());
        assert!(!trie.contains_key("apple".chars()));
        assert!(!trie.contains_key("apply".chars()));
        assert!(trie.contains_key("banana".chars()));
    }

    #[test]
    fn test_substitute_prefix() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);
        trie.insert("apply".chars(), 2);
        trie.insert("banana".chars(), 3);

        let trie = trie
            .substitute_prefix("app".chars(), "ora".chars())
            .unwrap();

        assert!(!trie.contains_key("apple".chars()));
        assert!(!trie.contains_key("apply".chars()));
        assert_eq!(trie.get("orale".chars()), Some(&1));
        assert_eq!(trie.get("oraly".chars()), Some(&2));
        assert!(trie.contains_key("banana".chars()));
    }

    #[test]
    fn test_substitute_prefix_not_found() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);

        let trie2: Option<Trie<char, i32>> =
            trie.substitute_prefix("orange".chars(), "pear".chars());
        assert!(trie2.is_none());
    }

    #[test]
    fn test_substitute_prefix_empty() {
        let mut trie = Trie::new();
        trie.insert("abc".chars(), 1);

        let trie2 = trie.substitute_prefix("".chars(), "X".chars()).unwrap();
        assert!(trie2.contains_key("Xabc".chars()));
        assert!(!trie2.contains_key("abc".chars()));
    }

    #[test]
    fn test_is_subset_and_superset() {
        let mut t1 = Trie::new();
        t1.insert("app".chars(), 1);
        t1.insert("apple".chars(), 2);

        let mut t2 = Trie::new();
        t2.insert("app".chars(), 1);
        t2.insert("apple".chars(), 2);
        t2.insert("banana".chars(), 3);

        assert!(t1.is_subset(&t2));
        assert!(t2.is_superset(&t1));
        assert!(!t2.is_subset(&t1));
        assert!(!t1.is_superset(&t2));

        let mut t3 = t1.clone();
        assert!(t1.is_subset(&t3));
        assert!(t3.is_subset(&t1));

        t3.insert("app".chars(), 10);
        assert!(!t3.is_subset(&t1));
        assert!(!t1.is_subset(&t3));
    }

    #[test]
    fn test_subset_empty() {
        let empty: Trie<char, i32> = Trie::new();
        let mut t1 = Trie::new();
        t1.insert("abc".chars(), 1);

        assert!(empty.is_subset(&t1));
        assert!(!t1.is_subset(&empty));
        assert!(t1.is_superset(&empty));
    }

    #[test]
    fn test_partial_ord() {
        let mut t1 = Trie::new();
        t1.insert("a".chars(), 1);

        let mut t2 = Trie::new();
        t2.insert("a".chars(), 1);
        t2.insert("b".chars(), 2);

        let mut t3 = Trie::new();
        t3.insert("c".chars(), 3);

        assert!(t1 < t2);
        assert!(t2 > t1);
        assert!(t1 <= t2);
        assert!(t2 >= t1);
        assert_eq!(t1.partial_cmp(&t1), Some(std::cmp::Ordering::Equal));

        // Incomparable
        assert_eq!(t1.partial_cmp(&t3), None);
        assert!(!(t1 < t3));
        assert!(!(t1 > t3));
        assert!(!(t1 <= t3));
        assert!(!(t1 >= t3));

        // Different values for same key
        let mut t4 = Trie::new();
        t4.insert("a".chars(), 2);
        assert_eq!(t1.partial_cmp(&t4), None);
    }

    #[test]
    #[cfg(feature = "serde")]
    fn test_serde() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);
        trie.insert("apply".chars(), 2);
        trie.insert("banana".chars(), 3);

        let serialized = serde_json::to_string(&trie).unwrap();
        let deserialized: Trie<char, i32> = serde_json::from_str(&serialized).unwrap();

        assert_eq!(trie, deserialized);
        assert_eq!(deserialized.get("apple".chars()), Some(&1));
        assert_eq!(deserialized.get("apply".chars()), Some(&2));
        assert_eq!(deserialized.get("banana".chars()), Some(&3));
    }

    #[test]
    fn test_filter() {
        let mut trie = Trie::new();
        trie.insert("apple".chars(), 1);
        trie.insert("apply".chars(), 2);
        trie.insert("banana".chars(), 3);
        trie.insert("app".chars(), 4);

        // Filter for values > 2
        let filtered = trie.filter(|_, &v| v > 2);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered.get("banana".chars()), Some(&3));
        assert_eq!(filtered.get("app".chars()), Some(&4));
        assert!(!filtered.contains_key("apple".chars()));

        // Filter for sequences starting with 'a'
        let filtered2 = trie.filter(|seq, _| seq.starts_with(&['a']));
        assert_eq!(filtered2.len(), 3);
        assert!(filtered2.contains_key("apple".chars()));
        assert!(filtered2.contains_key("apply".chars()));
        assert!(filtered2.contains_key("app".chars()));
        assert!(!filtered2.contains_key("banana".chars()));
    }

    #[test]
    fn test_filter_empty() {
        let trie: Trie<char, i32> = Trie::new();
        let filtered = trie.filter(|_, _| true);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_all_out() {
        let mut trie = Trie::new();
        trie.insert("abc".chars(), 1);
        let filtered = trie.filter(|_, _| false);
        assert!(filtered.is_empty());
    }
}
