use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::str::FromStr;

use internment::ArcIntern;
use serde::{Deserialize, Serialize};
use trie::Trie;

use ctadl_ir::mir::{self, Offset};

pub type Str = ArcIntern<str>;

/// A sequence of field/array accesses
///
/// The path dereferences go left to right
/// ["foo", "bar", "baz"] represents .foo.bar.baz
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Path(pub VecDeque<mir::FieldAccess>);

impl Path {
    /// Creates an empty path
    #[inline]
    pub fn empty() -> Self {
        Path(VecDeque::new())
    }

    /// Denotes the empty path
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns the string representation with dot prefixes for display
    /// e.g., ["foo", "bar"] becomes ".foo.bar"
    pub fn to_dot_string(&self) -> String {
        if self.0.is_empty() {
            String::new()
        } else {
            let mut result = String::with_capacity(self.0.len() * 2); // Rough estimate

            // Add leading dot for the whole path
            result.push('.');

            for (i, component) in self.0.iter().enumerate() {
                if i > 0 {
                    // Add separator dot (unescaped)
                    result.push('.');
                }

                // Handle both Symbol and Offset variants
                match component {
                    mir::FieldAccess::Symbol(symbol) => {
                        // Escape dots WITHIN components
                        let symbol_str: &str = symbol.as_ref();
                        let escaped = symbol_str.replace(".", "\\.");
                        result.push_str(&escaped);
                    }
                    mir::FieldAccess::Offset(offset) => {
                        result.push_str(&format!("[{}]", offset.0));
                    }
                }
            }

            result
        }
    }

    /// Concatenates two paths by combining their components, merging adjacent offsets.
    #[inline]
    pub fn concat(&self, other: &Path) -> Self {
        let mut result = self.clone();
        result.extend_merging(other.0.iter().cloned());
        result
    }

    /// Pushes a new component to the path, merging offsets if possible.
    pub fn push(&mut self, component: mir::FieldAccess) {
        if let (mir::FieldAccess::Offset(new_off), Some(mir::FieldAccess::Offset(last_off))) =
            (&component, self.0.back_mut())
        {
            last_off.0 += new_off.0;
            return;
        }
        self.0.push_back(component);
    }

    pub fn pop(mut self) -> Option<Self> {
        self.0.pop_back().map(|_| self)
    }

    /// Appends components from an iterator, merging offsets.
    pub fn extend_merging(&mut self, iter: impl IntoIterator<Item = mir::FieldAccess>) {
        for component in iter {
            self.push(component);
        }
    }

    /// Substitutes given prefix of path with new_prefix and returns the new path.
    /// self is ["p2", "p3"]
    /// prefix is ["p2"]
    /// new_prefix is ["p1"]
    /// result is ["p1", "p3"] (if p2 matches prefix of self)
    #[inline(always)]
    pub fn substitute_prefix(&self, prefix: &Path, new_prefix: &Path) -> Option<Path> {
        match_prefix(self, prefix).map(|suffix| {
            let mut result = new_prefix.clone();
            result.extend_merging(suffix);
            result
        })
    }

    /// Same as substitute_prefix but only returns a new path if the suffix after prefix matching
    /// is non-empty. Also merges offsets like substitute_prefix.
    #[inline]
    pub fn substitute_prefix_with_nonempty_suffix(
        &self,
        prefix: &Path,
        new_prefix: &Path,
    ) -> Option<Path> {
        match_prefix(self, prefix)
            .filter(|s| !s.is_empty())
            .map(|suffix| {
                let mut result = new_prefix.clone();
                result.extend_merging(suffix);
                result
            })
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path({})", self.to_dot_string())
    }
}

impl From<&mir::FieldAccesses> for Path {
    #[inline]
    fn from(path: &mir::FieldAccesses) -> Self {
        let mut p = Path::empty();
        p.extend_merging(path.iter().cloned());
        p
    }
}

impl From<&[&str]> for Path {
    #[inline]
    fn from(path: &[&str]) -> Self {
        let mut p = Path::empty();
        p.extend_merging(
            path.iter()
                .map(|&fld| mir::FieldAccess::Symbol(ArcIntern::from(fld))),
        );
        p
    }
}

impl<S: AsRef<str>> FromIterator<S> for Path {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut p = Path::empty();
        p.extend_merging(
            iter.into_iter()
                .map(|fld| mir::FieldAccess::Symbol(ArcIntern::from(fld.as_ref()))),
        );
        p
    }
}

impl From<&str> for Path {
    fn from(s: &str) -> Self {
        let components = parse_path_string(s);
        Path(components.into())
    }
}

impl FromStr for Path {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let components = parse_path_string(s);
        Ok(Path(components.into()))
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Nothing;

#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct AccessPathSet(pub Trie<mir::FieldAccess, Nothing>);

impl PartialOrd for AccessPathSet {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self == other {
            return Some(std::cmp::Ordering::Equal);
        }

        // Check if self is a subset of other: self union other == other
        if self.0.is_subset(&other.0) {
            return Some(std::cmp::Ordering::Less);
        }

        // Check if other is a subset of self: other union self == self
        if other.0.is_subset(&self.0) {
            return Some(std::cmp::Ordering::Greater);
        }

        None
    }
}

impl AccessPathSet {
    pub fn new() -> Self {
        Self(Trie::new())
    }

    pub fn singleton(path: Path) -> Self {
        let mut set = Self::new();
        set.insert(&path);
        set
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.0.contains_key(path.0.iter().cloned())
    }

    pub fn contains_empty_path(&self) -> bool {
        self.0.is_terminal()
    }

    pub fn insert(&mut self, path: &Path) {
        self.0.insert(path.0.iter().cloned(), Nothing);
    }

    pub fn union(&mut self, other: &Self) {
        self.0.union(&other.0);
    }

    pub fn iter(&self) -> impl Iterator<Item = Path> + '_ {
        self.0
            .all_sequences()
            .into_iter()
            .map(|(seq, _)| Path(seq.into()))
    }

    pub fn substitute_prefix(&self, prefix: &Path, new_prefix: &Path) -> Option<Self> {
        // We get all sequences from the trie and check which match the prefix using our
        // offset-aware match_prefix
        let mut result = Self::new();
        let mut found = false;

        for (seq, _) in self.0.all_sequences() {
            let path = Path(seq.into());
            if let Some(suffix) = match_prefix(&path, prefix) {
                found = true;
                let mut substituted_path = new_prefix.clone();
                substituted_path.extend_merging(suffix);
                result.insert(&substituted_path);
            }
        }

        if found { Some(result) } else { None }
    }
    pub fn substitute_prefix_with_nonempty_suffix(
        &self,
        prefix: &Path,
        new_prefix: &Path,
    ) -> Option<Self> {
        let mut result = Self::new();
        let mut found = false;

        for (seq, _) in self.0.all_sequences() {
            let path = Path(seq.into());
            if let Some(suffix) = match_prefix(&path, prefix).filter(|s| !s.is_empty()) {
                found = true;
                let mut substituted_path = new_prefix.clone();
                substituted_path.extend_merging(suffix);
                result.insert(&substituted_path);
            }
        }

        if found { Some(result) } else { None }
    }

    pub fn concat(&self, other: &Path) -> Self {
        let mut result = Self::new();

        for (seq, _) in self.0.all_sequences() {
            let mut path = Path(seq.into());
            path.extend_merging(other.0.iter().cloned());
            result.insert(&path);
        }

        result
    }

    pub fn pop(&self) -> Self {
        let mut result = Self::new();

        for (seq, _) in self.0.all_sequences() {
            let path = Path(seq.into());
            if let Some(popped_path) = path.pop() {
                result.insert(&popped_path);
            } else {
                result.insert(&Path::empty());
            }
        }

        result
    }
}

/// A map from access paths to access path sets.
///
/// Conceptually, this is a map [K -> [K -> ()]], where K is a trie.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct AccessPathMap(Trie<mir::FieldAccess, AccessPathSet>);

impl PartialOrd for AccessPathMap {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self == other {
            return Some(std::cmp::Ordering::Equal);
        }

        // Subset check for map: self <= other iff forall k in self, self[k] <= other[k]
        let mut self_le_other = true;
        for (seq, p2s) in self.0.all_sequences() {
            match other.0.get(seq) {
                Some(other_p2s) => {
                    if p2s.partial_cmp(other_p2s) == Some(std::cmp::Ordering::Greater)
                        || p2s.partial_cmp(other_p2s).is_none()
                    {
                        self_le_other = false;
                        break;
                    }
                }
                None => {
                    self_le_other = false;
                    break;
                }
            }
        }

        let mut other_le_self = true;
        for (seq, p2s) in other.0.all_sequences() {
            match self.0.get(seq) {
                Some(self_p2s) => {
                    if p2s.partial_cmp(self_p2s) == Some(std::cmp::Ordering::Greater)
                        || p2s.partial_cmp(self_p2s).is_none()
                    {
                        other_le_self = false;
                        break;
                    }
                }
                None => {
                    other_le_self = false;
                    break;
                }
            }
        }

        match (self_le_other, other_le_self) {
            (true, true) => Some(std::cmp::Ordering::Equal),
            (true, false) => Some(std::cmp::Ordering::Less),
            (false, true) => Some(std::cmp::Ordering::Greater),
            (false, false) => None,
        }
    }
}

impl AccessPathMap {
    pub fn new() -> Self {
        Self(Trie::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn singleton(p1: Path, p2: Path) -> Self {
        let mut res = Self::new();
        res.insert(p1, p2);
        res
    }

    pub fn insert(&mut self, p1: Path, p2: Path) {
        let mut set = self
            .0
            .get(p1.0.iter().cloned())
            .cloned()
            .unwrap_or_default();
        set.insert(&p2);
        self.0.insert(p1.0.iter().cloned(), set);
    }

    pub fn union(&mut self, other: &Self) {
        if self == other {
            return;
        }
        for (seq, set) in other.0.all_sequences() {
            let mut existing = self.0.get(&seq).cloned().unwrap_or_default();
            existing.union(&set);
            self.0.insert(seq, existing);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Path, Path)> + '_ {
        self.0.all_sequences().into_iter().flat_map(|(seq1, set)| {
            let p1 = Path(seq1.into());
            set.0
                .all_sequences()
                .into_iter()
                .map(move |(seq2, _)| (p1.clone(), Path(seq2.into())))
        })
    }

    /// Prefix substitution on sets of access paths
    ///
    /// - `paths`: Set of allowed access paths
    pub fn substitute_first_prefix(
        &self,
        prefix: &Path,
        new_prefix: &Path,
        paths: &AccessPathSet,
    ) -> Option<Self> {
        let mut result = Self::new();
        let mut found = false;

        for (seq1, p2s) in self.0.all_sequences() {
            let p1 = Path(seq1.into());
            if let Some(p1_new) = p1.substitute_prefix(prefix, new_prefix) {
                if paths.contains(&p1_new) {
                    found = true;
                    let mut existing = result
                        .0
                        .get(p1_new.0.iter().cloned())
                        .cloned()
                        .unwrap_or_default();
                    existing.union(&p2s);
                    result.0.insert(p1_new.0.iter().cloned(), existing);
                }
            }
        }

        if found { Some(result) } else { None }
    }

    pub fn apply_second_propagation(
        &self,
        p1: &Path,
        p23: &Path,
        paths: &AccessPathSet,
    ) -> Option<Self> {
        let mut result = Self::new();
        let mut found = false;

        for (seq2, p4s) in self.0.all_sequences() {
            let p2 = Path(seq2.into());
            for p4 in p4s.iter() {
                if let Some(p43) = p23.substitute_prefix(&p2, &p4) {
                    if paths.contains(&p43) {
                        found = true;
                        result.insert(p1.clone(), p43);
                    }
                }
            }
        }

        if found { Some(result) } else { None }
    }
}

// Ascent Lattice implementations

use ascent::lattice::Lattice;

impl Lattice for Path {
    fn meet(self, other: Self) -> Self {
        // Longest Common Prefix meet
        let mut res = VecDeque::new();
        for (a, b) in self.0.into_iter().zip(other.0.into_iter()) {
            if a == b {
                res.push_back(a);
            } else {
                break;
            }
        }
        Path(res)
    }

    fn join(self, other: Self) -> Self {
        // For Path, join is only possible if they are equal.
        if self == other { self } else { self }
    }

    fn meet_mut(&mut self, other: Self) -> bool {
        let met = self.clone().meet(other);
        if met != *self {
            *self = met;
            true
        } else {
            false
        }
    }

    fn join_mut(&mut self, other: Self) -> bool {
        let joined = self.clone().join(other);
        if joined != *self {
            *self = joined;
            true
        } else {
            false
        }
    }
}

impl Lattice for AccessPathSet {
    fn meet(mut self, other: Self) -> Self {
        self.0.intersection(&other.0);
        self
    }

    fn join(mut self, other: Self) -> Self {
        self.0.union(&other.0);
        self
    }

    fn meet_mut(&mut self, other: Self) -> bool {
        let old = self.clone();
        self.0.intersection(&other.0);
        *self != old
    }

    fn join_mut(&mut self, other: Self) -> bool {
        let old = self.clone();
        self.0.union(&other.0);
        *self != old
    }
}

impl Lattice for AccessPathMap {
    fn meet(self, other: Self) -> Self {
        let mut result = Self::new();
        for (seq, p2s) in self.0.all_sequences() {
            if let Some(other_p2s) = other.0.get(&seq) {
                let met_p2s = p2s.meet(other_p2s.clone());
                if !met_p2s.is_empty() {
                    result.0.insert(seq, met_p2s);
                }
            }
        }
        result
    }

    fn join(mut self, other: Self) -> Self {
        self.union(&other);
        self
    }

    fn meet_mut(&mut self, other: Self) -> bool {
        let old = self.clone();
        let met = self.clone().meet(other);
        if met != old {
            *self = met;
            true
        } else {
            false
        }
    }

    fn join_mut(&mut self, other: Self) -> bool {
        let old = self.clone();
        self.union(&other);
        *self != old
    }
}

/// Parses a path string into components, handling dot prefixes and escaped dots
pub fn parse_path_string(s: &str) -> Vec<mir::FieldAccess> {
    let s = s.trim_start_matches('.'); // Remove leading dot if present
    if s.is_empty() {
        return Vec::new();
    }

    let mut path = Path::empty();
    let mut current_component = String::new();
    let mut chars = s.chars().peekable();

    // The iteration logic needs to advance the iterator inside the loop, so, skip the clippy
    // warning.
    #[allow(clippy::while_let_on_iterator)]
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            // Handle escaped character
            if let Some(next_ch) = chars.next() {
                // This is an escaped character - add it to the current component
                current_component.push(next_ch);
            }
        } else if ch == '[' {
            // Handle offset notation like [42]
            if !current_component.is_empty() {
                path.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
                current_component = String::new();
            }
            let mut offset_str = String::new();
            #[allow(clippy::while_let_on_iterator)]
            while let Some(ch) = chars.next() {
                if ch == ']' {
                    if let Ok(offset) = offset_str.parse::<i64>() {
                        path.push(mir::FieldAccess::Offset(Offset(offset)));
                    }
                    break;
                }
                offset_str.push(ch);
            }
        } else if ch == '.' {
            // This is a separator dot - end of component
            if !current_component.is_empty() {
                path.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
                current_component = String::new();
            }
        } else {
            current_component.push(ch);
        }
    }

    // Add the last component if it's not empty
    if !current_component.is_empty() {
        path.push(mir::FieldAccess::Symbol(ArcIntern::from(current_component)));
    }

    path.0.into()
}

/// Returns the suffix solving the equation ap = prefix + suffix, if there is one. The suffix may
/// be empty. Otherwise returns none.
///
/// This supports offset arithmetic. For example, if ap = .x.[2] and prefix = .x.[1],
/// the suffix is .[1].
#[inline]
pub fn match_prefix(ap: &Path, prefix: &Path) -> Option<VecDeque<mir::FieldAccess>> {
    use mir::FieldAccess;
    use mir::Offset;
    let (ap_comps, prefix_comps) = (&ap.0, &prefix.0);

    if prefix_comps.is_empty() {
        return Some(ap_comps.clone());
    }

    if ap_comps.len() < prefix_comps.len() {
        return None;
    }

    // Check that all components except the last one match exactly
    for i in 0..prefix_comps.len() - 1 {
        if ap_comps[i] != prefix_comps[i] {
            return None;
        }
    }

    let last_idx = prefix_comps.len() - 1;
    match (&ap_comps[last_idx], &prefix_comps[last_idx]) {
        (FieldAccess::Offset(Offset(an)), FieldAccess::Offset(Offset(pn))) => {
            let mut suffix = VecDeque::new();
            let diff = an - pn;
            // Include an Offset in the suffix
            suffix.push_back(FieldAccess::Offset(Offset(diff)));
            // Append the remaining components of ap
            for comp in ap_comps.iter().skip(prefix_comps.len()) {
                suffix.push_back(comp.clone());
            }
            Some(suffix)
        }
        (a, p) if a == p => {
            // Exact match for the last prefix component
            Some(ap_comps.range(prefix_comps.len()..).cloned().collect())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn test_substitute_prefix() {
        let p: Path = Path::empty();
        assert_eq!(p, p.substitute_prefix(&p, &p).unwrap());

        let p: Path = ["a", "c"].iter().collect();
        let q: Path = ["a"].iter().collect();
        let r: Path = ["b"].iter().collect();
        let e: Path = ["b", "c"].iter().collect();

        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());

        let p: Path = ["a", "b"].iter().collect();
        let q: Path = ["c", "d"].iter().collect();
        assert!(p.substitute_prefix(&q, &Path::empty()).is_none());

        // Test case: p23.substitute_prefix(p2, p1) where p23=.[1], p2='', p1=.[1] -> .[2]
        // This tests offset merging when matching empty prefix
        use ctadl_ir::mir::{FieldAccess, Offset};

        // Create p23 = .[1]
        let mut p23_components = VecDeque::new();
        p23_components.push_back(FieldAccess::Offset(Offset(1)));
        let p23 = Path(p23_components);

        // Create p2 = '' (empty path)
        let p2 = Path::empty();

        // Create p1 = .[1]
        let mut p1_components = VecDeque::new();
        p1_components.push_back(FieldAccess::Offset(Offset(1)));
        let p1 = Path(p1_components);

        let result = p23.substitute_prefix(&p2, &p1).unwrap();

        // Create expected = .[2]
        let mut expected_components = VecDeque::new();
        expected_components.push_back(FieldAccess::Offset(Offset(2)));
        let expected = Path(expected_components);

        assert_eq!(result, expected);

        // More offset arithmetic tests
        let p: Path = ".x.[2]".into();
        let q: Path = ".x.[1]".into();
        let r: Path = ".y".into();
        let e: Path = ".y.[1]".into();
        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());

        let p: Path = ".x.[1].f".into();
        let q: Path = ".x".into();
        let r: Path = ".y".into();
        let e: Path = ".y.[1].f".into();
        assert_eq!(e, p.substitute_prefix(&q, &r).unwrap());
    }

    #[test]
    fn test_path_serialization() {
        let path: Path = ["foo", "bar.baz"].iter().collect();
        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.bar\\.baz");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_path_with_dots() {
        let path: Path = ["foo.bar", "baz.qux"].iter().collect();
        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo\\.bar.baz\\.qux");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }

    #[test]
    fn test_path_with_offsets() {
        // Test path with numeric offsets
        // Create a path manually with mixed FieldAccess types
        use ctadl_ir::mir::{FieldAccess, Offset};
        let mut path_components = VecDeque::new();
        path_components.push_back(FieldAccess::Symbol(ArcIntern::from("foo")));
        path_components.push_back(FieldAccess::Offset(Offset(42)));
        path_components.push_back(FieldAccess::Symbol(ArcIntern::from("bar")));
        let path = Path(path_components);

        let serialized = path.to_dot_string();
        assert_eq!(serialized, ".foo.[42].bar");

        let parsed_back: Path = serialized.parse().unwrap();
        assert_eq!(path, parsed_back);
    }
}
