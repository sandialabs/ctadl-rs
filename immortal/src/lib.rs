use hashbrown::HashTable;
use parking_lot::RwLock;
use rustc_hash::FxBuildHasher;
use std::hash::{BuildHasher, Hash};

pub struct Interner<T: ?Sized + 'static> {
    state: FxBuildHasher,
    shards: Box<[RwLock<HashTable<&'static T>>]>,
}

impl<T: ?Sized + Hash + Eq + Send + Sync + 'static> Interner<T> {
    pub fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(RwLock::new(HashTable::new()));
        }
        Self {
            state: FxBuildHasher,
            shards: shards.into_boxed_slice(),
        }
    }

    #[inline]
    fn shard_index(&self, hash: u64) -> usize {
        // Shard on the HIGH bits: hashbrown indexes its buckets with the LOW bits of
        // the same hash, so sharding on the high bits keeps full entropy inside each
        // shard's table (sharding on low bits would collapse every entry in a shard
        // into the same handful of hashbrown buckets).
        (hash >> 57) as usize % self.shards.len()
    }

    pub fn intern(&self, value: &T) -> &'static T
    where
        T: ToOwned,
        Box<T>: From<T::Owned>,
    {
        let hash = self.state.hash_one(value);
        let shard = &self.shards[self.shard_index(hash)];

        {
            let read = shard.read();
            if let Some(&existing) = read.find(hash, |&x| *x == *value) {
                return existing;
            }
        }

        let mut write = shard.write();
        if let Some(&existing) = write.find(hash, |&x| *x == *value) {
            return existing;
        }

        let owned = value.to_owned();
        let boxed = Box::<T>::from(owned);
        let leaked: &'static T = Box::leak(boxed);
        write.insert_unique(hash, leaked, |&x| self.state.hash_one(x));
        leaked
    }
}

/// Defines a new interned type.
///
/// This macro creates a transparent wrapper around a static reference to the interned type.
/// It implements `Hash`, `PartialEq`, and `Eq` using pointer identity, which is safe
/// because the interner ensures that equal values have the same address.
#[macro_export]
macro_rules! immortal {
    ($(#[$meta:meta])* $vis:vis struct $name:ident($inner:ty)) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialOrd, Ord)]
        #[repr(transparent)]
        $vis struct $name(&'static $inner);

        impl $name {
            /// Interns a value into a thread-local static interner for this type.
            #[inline]
            $vis fn intern(val: &$inner) -> Self {
                static INTERNER: ::std::sync::OnceLock<$crate::Interner<$inner>> = ::std::sync::OnceLock::new();
                $name(INTERNER.get_or_init(|| $crate::Interner::new(64)).intern(val))
            }
        }

        impl ::std::hash::Hash for $name {
            #[inline]
            fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {
                (self.0 as *const $inner).hash(state);
            }
        }

        impl ::std::cmp::PartialEq for $name {
            #[inline]
            fn eq(&self, other: &Self) -> bool {
                ::std::ptr::eq(self.0, other.0)
            }
        }

        impl ::std::cmp::Eq for $name {}

        impl ::std::ops::Deref for $name {
            type Target = $inner;
            #[inline]
            fn deref(&self) -> &Self::Target {
                self.0
            }
        }
    };
}

immortal! {
    /// A transparent wrapper around a `&'static str` that is interned.
    pub struct StringRef(str)
}

impl StringRef {
    pub fn as_str(&self) -> &'static str {
        self.0
    }

    pub fn new(s: &str) -> Self {
        Self::intern(s)
    }
}

impl std::fmt::Display for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.write_str(self.0)
    }
}

impl From<&str> for StringRef {
    fn from(s: &str) -> Self {
        Self::intern(s)
    }
}

impl From<String> for StringRef {
    fn from(s: String) -> Self {
        Self::intern(&s)
    }
}

impl AsRef<str> for StringRef {
    fn as_ref(&self) -> &str {
        self.0
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for StringRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.0)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for StringRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::Deserialize;
        let s = String::deserialize(deserializer)?;
        Ok(StringRef::intern(&s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interning() {
        let s1 = StringRef::new("hello");
        let s2 = StringRef::new("hello");
        let s3 = StringRef::new("world");

        assert_eq!(s1, s2);
        assert_ne!(s1, s3);

        // Check pointer equality
        assert!(std::ptr::eq(s1.as_str(), s2.as_str()));
        assert!(!std::ptr::eq(s1.as_str(), s3.as_str()));
    }

    #[test]
    fn test_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let s1 = StringRef::new("hello");
        let s2 = StringRef::new("hello");

        assert_eq!(s1, s2);

        let mut h1 = DefaultHasher::new();
        s1.hash(&mut h1);
        let hash1 = h1.finish();

        let mut h2 = DefaultHasher::new();
        s2.hash(&mut h2);
        let hash2 = h2.finish();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_generic_interning() {
        let interner = Interner::<[u8]>::new(4);
        let b1 = interner.intern(b"hello");
        let b2 = interner.intern(b"hello");
        assert!(std::ptr::eq(b1, b2));

        let interner2 = Interner::<i32>::new(4);
        let i1 = interner2.intern(&42);
        let i2 = interner2.intern(&42);
        assert!(std::ptr::eq(i1, i2));
    }

    #[test]
    fn test_deref() {
        let s = StringRef::new("hello");
        assert_eq!(&*s, "hello");
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn test_macro_custom_type() {
        immortal! {
            struct BytesRef([u8])
        }

        let b1 = BytesRef::intern(b"hello");
        let b2 = BytesRef::intern(b"hello");
        assert_eq!(b1, b2);
        assert!(std::ptr::eq(&*b1, &*b2));
    }
}
