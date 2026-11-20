//! Interned strings.

use rustc_hash::FxHashMap;

/// A table of interned strings: each distinct string gets one `u32`, and distinct strings get
/// distinct ones, so two atoms are equal exactly when their strings are. One table serves both
/// files of a comparison, so an atom means the same string in each.
#[derive(Debug, Default)]
pub struct Atoms {
    ids: FxHashMap<Box<str>, u32>,
    strings: Vec<Box<str>>,
}

impl Atoms {
    pub fn new() -> Self {
        Self::default()
    }

    /// The atom of `s`, made if `s` has none yet.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = u32::try_from(self.strings.len()).expect("more than u32::MAX distinct strings");
        self.strings.push(s.into());
        self.ids.insert(s.into(), id);
        id
    }

    /// The atom of `s`, if it has one.
    pub fn get_id(&self, s: &str) -> Option<u32> {
        self.ids.get(s).copied()
    }

    /// The string of `atom`.
    ///
    /// # Panics
    ///
    /// If `atom` was not made by this table.
    pub fn get(&self, atom: u32) -> &str {
        &self.strings[atom as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_is_injective_and_stable() {
        let mut atoms = Atoms::new();
        let a = atoms.intern("a");
        let b = atoms.intern("b");
        assert_ne!(a, b);
        assert_eq!(atoms.intern("a"), a);
        assert_eq!(atoms.get(a), "a");
        assert_eq!(atoms.get(b), "b");
        assert_eq!(atoms.get_id("b"), Some(b));
        assert_eq!(atoms.get_id("c"), None);
    }
}
