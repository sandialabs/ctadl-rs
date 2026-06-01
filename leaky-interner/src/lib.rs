use parking_lot::RwLock;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

struct StringInterner {
    shards: Box<[RwLock<HashSet<&'static str>>]>,
}

impl StringInterner {
    fn new(num_shards: usize) -> Self {
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            shards.push(RwLock::new(HashSet::new()));
        }
        Self {
            shards: shards.into_boxed_slice(),
        }
    }

    fn shard_for(&self, s: &str) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        let hash = hasher.finish();
        (hash % self.shards.len() as u64) as usize
    }

    fn intern(&self, s: &str) -> &'static str {
        let idx = self.shard_for(s);
        let shard = &self.shards[idx];

        {
            let read = shard.read();
            if let Some(&existing) = read.get(s) {
                return existing;
            }
        }

        let mut write = shard.write();
        if let Some(&existing) = write.get(s) {
            return existing;
        }

        let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
        write.insert(leaked);
        leaked
    }
}

static INTERNER: OnceLock<StringInterner> = OnceLock::new();

fn get_interner() -> &'static StringInterner {
    INTERNER.get_or_init(|| StringInterner::new(64))
}

/// A transparent wrapper around a `&'static str` that is interned.
#[derive(Debug, Clone, Copy, PartialOrd, Ord)]
#[repr(transparent)]
pub struct StringRef(&'static str);

impl Hash for StringRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0 as *const str).hash(state);
    }
}

impl PartialEq for StringRef {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for StringRef {}

impl StringRef {
    pub fn new(s: &str) -> Self {
        StringRef(get_interner().intern(s))
    }

    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl From<&str> for StringRef {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for StringRef {
    fn from(s: String) -> Self {
        Self::new(&s)
    }
}

impl std::ops::Deref for StringRef {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl AsRef<str> for StringRef {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl Borrow<str> for StringRef {
    fn borrow(&self) -> &str {
        self.0
    }
}

#[cfg(feature = "serde")]
impl Serialize for StringRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0)
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for StringRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(StringRef::new(&s))
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
    fn test_deref() {
        let s = StringRef::new("hello");
        assert_eq!(&*s, "hello");
        assert_eq!(s.len(), 5);
    }
}
