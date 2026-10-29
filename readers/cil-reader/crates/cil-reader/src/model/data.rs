//! Module-wide lazy side tables.
//!
//! Port of Cecil's `MetadataSystem` + the `InitializeXxx` passes in
//! `MetadataReader`: each map is built on first touch by one linear pass over
//! its table, stored in a `OnceLock`. All maps are lifetime-free (they store
//! rids, tokens and heap indices), which keeps `Module<'a>` covariant and
//! `Send + Sync`.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::model::module::Module;
use crate::model::sig::{SigReader, TypeSig};
use crate::raw::heaps::{BlobIndex, StringIndex};
use crate::raw::metadata::Metadata;
use crate::raw::pe::PeImage;
use crate::raw::tables::*;
use crate::token::{MetadataToken, TokenType};

/// A contiguous 1-based rid range (Cecil `Range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub start: u32,
    pub length: u32,
}

impl Range {
    pub fn iter(self) -> impl ExactSizeIterator<Item = u32> {
        self.start..self.start + self.length
    }
}

pub(crate) struct ModuleData<'a> {
    pub image: PeImage<'a>,
    pub md: Metadata<'a>,
    pub lazy: LazyTables,
}

type Lazy<T> = OnceLock<Result<T>>;

#[derive(Default)]
pub(crate) struct LazyTables {
    /// TypeDef rid → (fields range, methods range), index rid-1.
    pub type_member_ranges: Lazy<Vec<(Range, Range)>>,
    /// Declaring TypeDef rid → nested TypeDef rids (InitializeNestedTypes).
    pub nested_types: Lazy<NestedMaps>,
    /// TypeDef rid → (InterfaceImpl rid, interface token) (InitializeInterfaces).
    pub interfaces: Lazy<HashMap<u32, Vec<(u32, MetadataToken)>>>,
    /// TypeDef rid → (packing_size, class_size) (InitializeTypeLayouts).
    pub class_layouts: Lazy<HashMap<u32, (u16, u32)>>,
    /// Field rid → offset (InitializeFieldLayouts).
    pub field_layouts: Lazy<HashMap<u32, u32>>,
    /// Field rid → rva (InitializeFieldRVAs).
    pub field_rvas: Lazy<HashMap<u32, u32>>,
    /// Owner token → marshal blob (InitializeMarshalInfos).
    pub field_marshals: Lazy<HashMap<MetadataToken, BlobIndex>>,
    /// Owner token → (element type, value blob) (InitializeConstants).
    pub constants: Lazy<HashMap<MetadataToken, (u8, BlobIndex)>>,
    /// Method rid → overridden method tokens (InitializeOverrides).
    pub overrides: Lazy<HashMap<u32, Vec<MetadataToken>>>,
    /// Owner token → CustomAttribute rid ranges (InitializeCustomAttributes).
    pub custom_attribute_ranges: Lazy<HashMap<MetadataToken, Vec<Range>>>,
    /// Owner token → DeclSecurity rid ranges (InitializeSecurityDeclarations).
    pub security_ranges: Lazy<HashMap<MetadataToken, Vec<Range>>>,
    /// TypeDef rid → Event rid range (InitializeEvents).
    pub events_ranges: Lazy<HashMap<u32, Range>>,
    /// TypeDef rid → Property rid range (InitializeProperties).
    pub properties_ranges: Lazy<HashMap<u32, Range>>,
    /// Method rid → (semantics, association token); and the reverse
    /// (association token → methods) for property/event accessors
    /// (InitializeMethodSemantics).
    pub semantics: Lazy<SemanticsMaps>,
    /// Method rid → (flags, import name, ModuleRef rid) (InitializePInvokes).
    pub pinvokes: Lazy<HashMap<u32, (u16, StringIndex, u32)>>,
    /// Owner token → GenericParam rid ranges (InitializeGenericParameters).
    pub generic_param_ranges: Lazy<HashMap<MetadataToken, Vec<Range>>>,
    /// GenericParam rid → (GenericParamConstraint rid, constraint token)
    /// (InitializeGenericConstraints).
    pub generic_constraints: Lazy<HashMap<u32, Vec<(u32, MetadataToken)>>>,
    /// Field/Method/Property/Event rid → the TypeDef whose list reads it
    /// last, in that order (see [`ModuleData::member_owner`]).
    pub member_owners: [Lazy<HashMap<u32, u32>>; 4],
}

#[derive(Default)]
pub(crate) struct NestedMaps {
    pub nested: HashMap<u32, Vec<u32>>,
    pub reverse: HashMap<u32, u32>,
}

#[derive(Default)]
pub(crate) struct SemanticsMaps {
    /// Method rid → (attributes, association token), from the method's last
    /// MethodSemantics row, as Cecil keeps one.
    pub by_method: HashMap<u32, (u16, MetadataToken)>,
}

fn get<T>(slot: &Lazy<T>, build: impl FnOnce() -> Result<T>) -> Result<&T> {
    match slot.get_or_init(build) {
        Ok(v) => Ok(v),
        Err(e) => Err(e.clone()),
    }
}

impl<'a> ModuleData<'a> {
    /// The module these data are, for the APIs that take one.
    pub(crate) fn module(&self) -> &Module<'a> {
        // SAFETY: Module is a repr(transparent) wrapper of ModuleData, so a
        // reference to one is a valid reference to the other.
        unsafe { &*std::ptr::from_ref(self).cast::<Module<'a>>() }
    }

    /// TypeSpec `rid`'s signature, for a token met at signature level `depth`
    /// (see [`SigReader::below`]).
    pub(crate) fn type_spec_sig(&self, rid: u32, depth: u32) -> Result<TypeSig> {
        let row = self.md.row::<TypeSpecRaw>(rid)?;
        SigReader::below(self.md.blobs().view(row.signature)?, depth).type_sig()
    }

    /// TypeDef member list ranges, mirroring `MetadataReader.ReadListRange`:
    /// the length of a list column range comes from peeking the next row (or
    /// the member table's length + 1 for the last row, even when the list
    /// indexes a Ptr table).
    pub fn type_member_ranges(&self) -> Result<&Vec<(Range, Range)>> {
        get(&self.lazy.type_member_ranges, || {
            let table = self.md.table::<TypeDefRaw>();
            let count = table.len();
            // A table of u32::MAX rows cannot hold its last one, so
            // saturating loses nothing.
            let field_end = self.md.row_count(Table::Field).saturating_add(1);
            let method_end = self.md.row_count(Table::Method).saturating_add(1);

            let mut ranges = Vec::with_capacity(table.rows_present() as usize);
            let mut prev: Option<(u32, u32)> = None;
            for rid in 1..=count {
                let row = table.row(rid)?;
                if let Some((pf, pm)) = prev {
                    let last = ranges.len();
                    let _ = last;
                    ranges.push((
                        make_range(pf, row.field_list),
                        make_range(pm, row.method_list),
                    ));
                }
                prev = Some((row.field_list, row.method_list));
            }
            if let Some((pf, pm)) = prev {
                ranges.push((make_range(pf, field_end), make_range(pm, method_end)));
            }
            Ok(ranges)
        })
    }

    /// The member ranges of TypeDef `type_rid`; empty for a rid that names no
    /// row.
    fn member_ranges_of(&self, type_rid: u32) -> Result<(Range, Range)> {
        let ranges = self.type_member_ranges()?;
        Ok(type_rid
            .checked_sub(1)
            .and_then(|i| ranges.get(i as usize))
            .copied()
            .unwrap_or_default())
    }

    pub fn fields_range(&self, type_rid: u32) -> Result<Range> {
        Ok(self.member_ranges_of(type_rid)?.0)
    }

    pub fn methods_range(&self, type_rid: u32) -> Result<Range> {
        Ok(self.member_ranges_of(type_rid)?.1)
    }

    /// The rids of the members a list `range` reads (`ReadFields` and the
    /// others): through `ptr_table` when it has a row `range.start`, and
    /// straight from the member table otherwise, as Cecil falls back to.
    /// Rows past the end of the table read are left out, where Cecil reads
    /// them at the wrong offset, as are Ptr rows naming rid 0 or a member
    /// past its table. A range into rows the header claims but the tables
    /// stream does not hold is an error, as Cecil throws reading them.
    pub fn member_rids(&self, range: Range, ptr_table: Table) -> Result<Vec<u32>> {
        let target = member_table(ptr_table);
        let ptr_rows = self.md.row_count(ptr_table);
        let target_rows = self.md.row_count(target);
        let through_ptr = ptr_rows != 0 && range.start <= ptr_rows;
        let indexed = if through_ptr { ptr_table } else { target };
        let end = (range.start + range.length).min(self.md.row_count(indexed).saturating_add(1));
        if end > self.md.rows_present(indexed).saturating_add(1) {
            return Err(Error::bad(0, "member list runs past the tables stream"));
        }
        let mut out = Vec::with_capacity(range.length.min(4096) as usize);
        for rid in range.start..end {
            let actual = if through_ptr {
                match ptr_table {
                    Table::FieldPtr => self.md.row::<FieldPtrRaw>(rid)?.field,
                    Table::MethodPtr => self.md.row::<MethodPtrRaw>(rid)?.method,
                    Table::EventPtr => self.md.row::<EventPtrRaw>(rid)?.event,
                    _ => self.md.row::<PropertyPtrRaw>(rid)?.property,
                }
            } else {
                rid
            };
            if (1..=target_rows).contains(&actual) {
                out.push(actual);
            }
        }
        Ok(out)
    }

    /// The TypeDef whose member list reads member `rid` of the table
    /// `ptr_table` points to. Cecil sets `DeclaringType` from the
    /// collection that read the member. When a FieldPtr or MethodPtr table
    /// exists it reads every type's fields and methods up front, in rid
    /// order (`CompleteTypes`), so a field or method that two lists read
    /// takes the later type, as do properties and events here; otherwise
    /// it finds a field's or method's type by a binary search of the
    /// ranges (`MetadataSystem.BinaryRangeSearch`).
    pub fn member_owner(&self, ptr_table: Table, rid: u32) -> Result<Option<u32>> {
        let field = match ptr_table {
            Table::FieldPtr => true,
            Table::MethodPtr => false,
            _ => return Ok(self.member_owners(ptr_table)?.get(&rid).copied()),
        };
        if self.completes_types() {
            return Ok(self.member_owners(ptr_table)?.get(&rid).copied());
        }
        let ranges = self.type_member_ranges()?;
        let mut min = 0usize;
        let mut max = ranges.len().wrapping_sub(1);
        while min <= max && max != usize::MAX {
            let mid = min + (max - min) / 2;
            let range = if field { ranges[mid].0 } else { ranges[mid].1 };
            if rid < range.start {
                if mid == 0 {
                    break;
                }
                max = mid - 1;
            } else if rid >= range.start + range.length {
                min = mid + 1;
            } else {
                return Ok(Some(mid as u32 + 1));
            }
        }
        Ok(None)
    }

    /// Whether Cecil reads every type's fields and methods up front, which
    /// it does when a FieldPtr or MethodPtr table exists (`CompleteTypes`).
    pub fn completes_types(&self) -> bool {
        self.md.row_count(Table::FieldPtr) != 0 || self.md.row_count(Table::MethodPtr) != 0
    }

    /// Member rid → the last TypeDef, in rid order, whose list reads it.
    fn member_owners(&self, ptr_table: Table) -> Result<&HashMap<u32, u32>> {
        let slot = match ptr_table {
            Table::FieldPtr => 0,
            Table::MethodPtr => 1,
            Table::PropertyPtr => 2,
            _ => 3,
        };
        get(&self.lazy.member_owners[slot], || {
            let mut lists: Vec<(u32, Range)> = match ptr_table {
                Table::FieldPtr | Table::MethodPtr => self
                    .type_member_ranges()?
                    .iter()
                    .zip(1..)
                    .map(|(&(fields, methods), owner)| {
                        (owner, if slot == 0 { fields } else { methods })
                    })
                    .collect(),
                Table::PropertyPtr => self
                    .properties_ranges()?
                    .iter()
                    .map(|(&o, &r)| (o, r))
                    .collect(),
                _ => self
                    .events_ranges()?
                    .iter()
                    .map(|(&o, &r)| (o, r))
                    .collect(),
            };
            lists.sort_unstable_by_key(|&(owner, _)| owner);
            let mut owners = HashMap::new();
            for (owner, range) in lists {
                for rid in self.member_rids(range, ptr_table)? {
                    owners.insert(rid, owner);
                }
            }
            Ok(owners)
        })
    }

    pub fn nested_maps(&self) -> Result<&NestedMaps> {
        get(&self.lazy.nested_types, || {
            let mut maps = NestedMaps::default();
            for (_, row) in self.md.table::<NestedClassRaw>().rows() {
                let row = row?;
                maps.nested
                    .entry(row.enclosing_class)
                    .or_default()
                    .push(row.nested_class);
                maps.reverse.insert(row.nested_class, row.enclosing_class);
            }
            Ok(maps)
        })
    }

    /// Whether `rid` names a TypeDef row: not 0, and not past the table.
    pub fn is_type_def_rid(&self, rid: u32) -> bool {
        (1..=self.md.row_count(Table::TypeDef)).contains(&rid)
    }

    /// The enclosing type of TypeDef `rid`: its NestedClass row's, but only
    /// for a nested visibility, as Cecil sets `DeclaringType`
    /// (`MetadataReader.ReadType`). An enclosing rid past the TypeDef table
    /// is none, as `GetTypeDefinition` returns null there; so is rid 0, where
    /// Cecil throws.
    pub fn declaring_type_rid(&self, rid: u32) -> Result<Option<u32>> {
        let Some(&declaring) = self.nested_maps()?.reverse.get(&rid) else {
            return Ok(None);
        };
        if !self.is_type_def_rid(declaring) {
            return Ok(None);
        }
        let flags = self.md.row::<TypeDefRaw>(rid)?.flags;
        // VisibilityMask values 2-7 are the Nested* ones.
        let visibility = flags & crate::model::handles::type_attributes::VISIBILITY_MASK;
        Ok((visibility >= 2).then_some(declaring))
    }

    /// The TypeRef that TypeRef `rid`, whose ResolutionScope is `scope`,
    /// is nested in (`MetadataReader.ReadTypeReference`): none for a scope
    /// that is not a TypeRef, names `rid` itself (which obfuscated modules
    /// write, and Cecil scopes to the module), or names rid 0 or a rid past
    /// the TypeRef table, where `GetTypeReference` returns null.
    pub fn typeref_declaring_rid(&self, rid: u32, scope: MetadataToken) -> Option<u32> {
        (scope.token_type() == Some(TokenType::TypeRef)
            && scope.rid() != rid
            && (1..=self.md.row_count(Table::TypeRef)).contains(&scope.rid()))
        .then_some(scope.rid())
    }

    pub fn interfaces_map(&self) -> Result<&HashMap<u32, Vec<(u32, MetadataToken)>>> {
        get(&self.lazy.interfaces, || {
            let mut map: HashMap<u32, Vec<(u32, MetadataToken)>> = HashMap::new();
            for (rid, row) in self.md.table::<InterfaceImplRaw>().rows() {
                let row = row?;
                map.entry(row.class).or_default().push((rid, row.interface));
            }
            Ok(map)
        })
    }

    pub fn class_layouts(&self) -> Result<&HashMap<u32, (u16, u32)>> {
        get(&self.lazy.class_layouts, || {
            let mut map = HashMap::new();
            for (_, row) in self.md.table::<ClassLayoutRaw>().rows() {
                let row = row?;
                map.insert(row.parent, (row.packing_size, row.class_size));
            }
            Ok(map)
        })
    }

    pub fn field_layouts(&self) -> Result<&HashMap<u32, u32>> {
        get(&self.lazy.field_layouts, || {
            let mut map = HashMap::new();
            for (_, row) in self.md.table::<FieldLayoutRaw>().rows() {
                let row = row?;
                map.insert(row.field, row.offset);
            }
            Ok(map)
        })
    }

    pub fn field_rvas(&self) -> Result<&HashMap<u32, u32>> {
        get(&self.lazy.field_rvas, || {
            let mut map = HashMap::new();
            for (_, row) in self.md.table::<FieldRvaRaw>().rows() {
                let row = row?;
                map.insert(row.field, row.rva);
            }
            Ok(map)
        })
    }

    pub fn field_marshals(&self) -> Result<&HashMap<MetadataToken, BlobIndex>> {
        get(&self.lazy.field_marshals, || {
            let mut map = HashMap::new();
            for (_, row) in self.md.table::<FieldMarshalRaw>().rows() {
                let row = row?;
                map.insert(row.parent, row.native_type);
            }
            Ok(map)
        })
    }

    pub fn constants(&self) -> Result<&HashMap<MetadataToken, (u8, BlobIndex)>> {
        get(&self.lazy.constants, || {
            let mut map = HashMap::new();
            for (_, row) in self.md.table::<ConstantRaw>().rows() {
                let row = row?;
                map.insert(row.parent, ((row.ty & 0xff) as u8, row.value));
            }
            Ok(map)
        })
    }

    pub fn overrides(&self) -> Result<&HashMap<u32, Vec<MetadataToken>>> {
        get(&self.lazy.overrides, || {
            let mut map: HashMap<u32, Vec<MetadataToken>> = HashMap::new();
            for (_, row) in self.md.table::<MethodImplRaw>().rows() {
                let row = row?;
                if row.method_body.token_type() != Some(TokenType::Method) {
                    return Err(Error::Unsupported("MethodImpl body is not a MethodDef"));
                }
                map.entry(row.method_body.rid())
                    .or_default()
                    .push(row.method_declaration);
            }
            Ok(map)
        })
    }

    pub fn events_ranges(&self) -> Result<&HashMap<u32, Range>> {
        get(&self.lazy.events_ranges, || {
            self.map_ranges::<EventMapRaw>(Table::Event, |r| (r.parent, r.event_list))
        })
    }

    pub fn properties_ranges(&self) -> Result<&HashMap<u32, Range>> {
        get(&self.lazy.properties_ranges, || {
            self.map_ranges::<PropertyMapRaw>(Table::Property, |r| (r.parent, r.property_list))
        })
    }

    /// Shared EventMap/PropertyMap logic: each map row owns the range from its
    /// list column up to the next row's.
    fn map_ranges<R: RawRow>(
        &self,
        target: Table,
        cols: impl Fn(&R) -> (u32, u32),
    ) -> Result<HashMap<u32, Range>> {
        let table = self.md.table::<R>();
        let count = table.len();
        // The last row's range ends at the member table's length + 1, even
        // when the list indexes a Ptr table (`ReadListRange`).
        let end = self.md.row_count(target).saturating_add(1);
        let mut map = HashMap::with_capacity(table.rows_present() as usize);
        let mut prev: Option<(u32, u32)> = None;
        for rid in 1..=count {
            let row = table.row(rid)?;
            let (owner, start) = cols(&row);
            if let Some((powner, pstart)) = prev {
                map.insert(powner, make_range(pstart, start));
            }
            prev = Some((owner, start));
        }
        if let Some((powner, pstart)) = prev {
            map.insert(powner, make_range(pstart, end));
        }
        Ok(map)
    }

    pub fn semantics(&self) -> Result<&SemanticsMaps> {
        get(&self.lazy.semantics, || {
            let mut maps = SemanticsMaps::default();
            for (_, row) in self.md.table::<MethodSemanticsRaw>().rows() {
                let row = row?;
                maps.by_method
                    .insert(row.method, (row.semantics, row.association));
            }
            Ok(maps)
        })
    }

    pub fn pinvokes(&self) -> Result<&HashMap<u32, (u16, StringIndex, u32)>> {
        get(&self.lazy.pinvokes, || {
            let mut map = HashMap::new();
            for (_, row) in self.md.table::<ImplMapRaw>().rows() {
                let row = row?;
                if row.member_forwarded.token_type() != Some(TokenType::Method) {
                    continue; // InitializePInvokes skips non-method rows
                }
                map.insert(
                    row.member_forwarded.rid(),
                    (row.mapping_flags, row.import_name, row.import_scope),
                );
            }
            Ok(map)
        })
    }

    /// `MetadataReader.InitializeRanges` over the GenericParam table: rows are
    /// grouped by owner; each owner may get several disjoint ranges.
    pub fn generic_param_ranges(&self) -> Result<&HashMap<MetadataToken, Vec<Range>>> {
        get(&self.lazy.generic_param_ranges, || {
            let table = self.md.table::<GenericParamRaw>();
            let mut ranges: HashMap<MetadataToken, Vec<Range>> = HashMap::new();
            let mut owner = MetadataToken::ZERO;
            let mut range = Range {
                start: 1,
                length: 0,
            };
            for (rid, row) in table.rows() {
                let next = row?.owner;
                if rid == 1 {
                    owner = next;
                    range.length += 1;
                } else if next != owner {
                    add_range(&mut ranges, owner, range);
                    range = Range {
                        start: rid,
                        length: 1,
                    };
                    owner = next;
                } else {
                    range.length += 1;
                }
            }
            if !table.is_empty() {
                add_range(&mut ranges, owner, range);
            }
            Ok(ranges)
        })
    }

    pub fn generic_constraints(&self) -> Result<&HashMap<u32, Vec<(u32, MetadataToken)>>> {
        get(&self.lazy.generic_constraints, || {
            let mut map: HashMap<u32, Vec<(u32, MetadataToken)>> = HashMap::new();
            for (rid, row) in self.md.table::<GenericParamConstraintRaw>().rows() {
                let row = row?;
                map.entry(row.owner)
                    .or_default()
                    .push((rid, row.constraint));
            }
            Ok(map)
        })
    }

    pub fn custom_attribute_ranges(&self) -> Result<&HashMap<MetadataToken, Vec<Range>>> {
        get(&self.lazy.custom_attribute_ranges, || {
            self.owner_ranges::<CustomAttributeRaw>(|r| r.parent)
        })
    }

    pub fn security_ranges(&self) -> Result<&HashMap<MetadataToken, Vec<Range>>> {
        get(&self.lazy.security_ranges, || {
            self.owner_ranges::<DeclSecurityRaw>(|r| r.parent)
        })
    }

    fn owner_ranges<R: RawRow>(
        &self,
        owner_of: impl Fn(&R) -> MetadataToken,
    ) -> Result<HashMap<MetadataToken, Vec<Range>>> {
        let table = self.md.table::<R>();
        let mut ranges: HashMap<MetadataToken, Vec<Range>> = HashMap::new();
        let mut owner = MetadataToken::ZERO;
        let mut range = Range {
            start: 1,
            length: 0,
        };
        for (rid, row) in table.rows() {
            let next = owner_of(&row?);
            if rid == 1 {
                owner = next;
                range.length += 1;
            } else if next != owner {
                add_range(&mut ranges, owner, range);
                range = Range {
                    start: rid,
                    length: 1,
                };
                owner = next;
            } else {
                range.length += 1;
            }
        }
        if !table.is_empty() {
            add_range(&mut ranges, owner, range);
        }
        Ok(ranges)
    }
}

/// The member table a Ptr table points into.
fn member_table(ptr_table: Table) -> Table {
    match ptr_table {
        Table::FieldPtr => Table::Field,
        Table::MethodPtr => Table::Method,
        Table::EventPtr => Table::Event,
        _ => Table::Property,
    }
}

fn make_range(start: u32, next: u32) -> Range {
    if start == 0 {
        Range::default()
    } else {
        Range {
            start,
            length: next.saturating_sub(start),
        }
    }
}

fn add_range(ranges: &mut HashMap<MetadataToken, Vec<Range>>, owner: MetadataToken, range: Range) {
    // Cecil AddRange drops ranges with owner rid 0.
    if owner.rid() == 0 {
        return;
    }
    ranges.entry(owner).or_default().push(range);
}
