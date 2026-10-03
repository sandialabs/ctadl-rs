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
    /// Field/Method/Property/Event rid → the FieldPtr/MethodPtr/PropertyPtr/
    /// EventPtr row naming it, in that order. Only built when the Ptr table
    /// exists, since member list columns then index the Ptr table.
    pub ptr_inverses: [Lazy<HashMap<u32, u32>>; 4],
}

#[derive(Default)]
pub(crate) struct NestedMaps {
    pub nested: HashMap<u32, Vec<u32>>,
    pub reverse: HashMap<u32, u32>,
}

#[derive(Default)]
pub(crate) struct SemanticsMaps {
    /// Method rid → (attributes, association token).
    pub by_method: HashMap<u32, (u16, MetadataToken)>,
    /// Association token (Event/Property) → (attributes, method rid) list.
    pub by_association: HashMap<MetadataToken, Vec<(u16, u32)>>,
}

fn get<T>(slot: &Lazy<T>, build: impl FnOnce() -> Result<T>) -> Result<&T> {
    match slot.get_or_init(build) {
        Ok(v) => Ok(v),
        Err(e) => Err(e.clone()),
    }
}

impl<'a> ModuleData<'a> {
    /// TypeDef member list ranges, mirroring `MetadataReader.ReadListRange`:
    /// the length of a list column range comes from peeking the next row (or
    /// the target table length + 1 for the last row).
    pub fn type_member_ranges(&self) -> Result<&Vec<(Range, Range)>> {
        get(&self.lazy.type_member_ranges, || {
            let table = self.md.table::<TypeDefRaw>();
            let count = table.len();
            let field_ptr = self.md.row_count(Table::FieldPtr);
            let method_ptr = self.md.row_count(Table::MethodPtr);
            // When Ptr tables exist, list columns index into them.
            let field_end = if field_ptr != 0 {
                field_ptr
            } else {
                self.md.row_count(Table::Field)
            } + 1;
            let method_end = if method_ptr != 0 {
                method_ptr
            } else {
                self.md.row_count(Table::Method)
            } + 1;

            let mut ranges = Vec::with_capacity(count as usize);
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

    pub fn fields_range(&self, type_rid: u32) -> Result<Range> {
        Ok(self
            .type_member_ranges()?
            .get(type_rid as usize - 1)
            .copied()
            .unwrap_or_default()
            .0)
    }

    pub fn methods_range(&self, type_rid: u32) -> Result<Range> {
        Ok(self
            .type_member_ranges()?
            .get(type_rid as usize - 1)
            .copied()
            .unwrap_or_default()
            .1)
    }

    /// The position of member `rid` in its owners' list columns: `rid` itself,
    /// or, when `ptr_table` exists, the Ptr row that names it. `None` when no
    /// Ptr row does, so no owner lists it.
    ///
    /// Cecil sets `DeclaringType` from the collection that read the member,
    /// through the Ptr table; if two Ptr rows name the same member, the last
    /// read wins, so the last row wins here too.
    pub fn member_list_index(&self, ptr_table: Table, rid: u32) -> Result<Option<u32>> {
        let slot = match ptr_table {
            Table::FieldPtr => 0,
            Table::MethodPtr => 1,
            Table::PropertyPtr => 2,
            Table::EventPtr => 3,
            // No other list column goes through a Ptr table that has an
            // inverse here.
            _ => return Ok(Some(rid)),
        };
        let ptr_rows = self.md.row_count(ptr_table);
        if ptr_rows == 0 {
            return Ok(Some(rid));
        }
        let inverse = get(&self.lazy.ptr_inverses[slot], || {
            let mut map = HashMap::new();
            for ptr_rid in 1..=ptr_rows {
                let target = match ptr_table {
                    Table::FieldPtr => self.md.row::<FieldPtrRaw>(ptr_rid)?.field,
                    Table::MethodPtr => self.md.row::<MethodPtrRaw>(ptr_rid)?.method,
                    Table::PropertyPtr => self.md.row::<PropertyPtrRaw>(ptr_rid)?.property,
                    _ => self.md.row::<EventPtrRaw>(ptr_rid)?.event,
                };
                map.insert(target, ptr_rid);
            }
            Ok(map)
        })?;
        Ok(inverse.get(&rid).copied())
    }

    /// Reverse owner lookup by binary search over the member ranges
    /// (`MetadataSystem.BinaryRangeSearch`).
    pub fn member_declaring_type(&self, member_rid: u32, field: bool) -> Result<Option<u32>> {
        let ptr_table = if field { Table::FieldPtr } else { Table::MethodPtr };
        let Some(member_rid) = self.member_list_index(ptr_table, member_rid)? else {
            return Ok(None);
        };
        let ranges = self.type_member_ranges()?;
        let mut min = 0usize;
        let mut max = ranges.len().wrapping_sub(1);
        while min <= max && max != usize::MAX {
            let mid = min + (max - min) / 2;
            let range = if field { ranges[mid].0 } else { ranges[mid].1 };
            if member_rid < range.start {
                if mid == 0 {
                    break;
                }
                max = mid - 1;
            } else if member_rid >= range.start + range.length {
                min = mid + 1;
            } else {
                return Ok(Some(mid as u32 + 1));
            }
        }
        Ok(None)
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
            self.map_ranges::<EventMapRaw>(Table::Event, Table::EventPtr, |r| {
                (r.parent, r.event_list)
            })
        })
    }

    pub fn properties_ranges(&self) -> Result<&HashMap<u32, Range>> {
        get(&self.lazy.properties_ranges, || {
            self.map_ranges::<PropertyMapRaw>(Table::Property, Table::PropertyPtr, |r| {
                (r.parent, r.property_list)
            })
        })
    }

    /// Shared EventMap/PropertyMap logic: each map row owns the range from its
    /// list column up to the next row's.
    fn map_ranges<R: RawRow>(
        &self,
        target: Table,
        target_ptr: Table,
        cols: impl Fn(&R) -> (u32, u32),
    ) -> Result<HashMap<u32, Range>> {
        let table = self.md.table::<R>();
        let count = table.len();
        let ptr_rows = self.md.row_count(target_ptr);
        let end = if ptr_rows != 0 {
            ptr_rows
        } else {
            self.md.row_count(target)
        } + 1;
        let mut map = HashMap::with_capacity(count as usize);
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
                maps.by_association
                    .entry(row.association)
                    .or_default()
                    .push((row.semantics, row.method));
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
