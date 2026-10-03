//! The metadata system: streams located, row counts read, per-image row sizes
//! and column offsets computed.
//!
//! Port of `ImageReader.ReadMetadata`/`ReadTableHeap`/`ComputeTableInformations`.

use crate::error::{Error, Result};
use crate::raw::coded::{CODED_INDEX_COUNT, CodedIndexKind};
use crate::raw::cursor::Cur;
use crate::raw::heaps::{BlobHeap, GuidHeap, StringsHeap, UserStringsHeap};
use crate::raw::pe::PeImage;
use crate::raw::tables::{RawRow, SizeCtx, TABLE_COUNT, Table, TableInfo, TableRef, table_row_size};

pub struct Metadata<'a> {
    pub(crate) strings_data: &'a [u8],
    pub(crate) blob_data: &'a [u8],
    pub(crate) us_data: &'a [u8],
    pub(crate) guid_data: &'a [u8],
    pub(crate) tables_data: &'a [u8],
    /// The `#Pdb` stream, for the optional portable-PDB phase (M8).
    #[allow(dead_code)]
    pub(crate) pdb_data: Option<&'a [u8]>,

    /// The `HeapSizes` byte from the `#~` header.
    pub heap_sizes: u8,
    /// The `Valid` bitvector.
    pub valid: u64,
    /// The `Sorted` bitvector.
    pub sorted: u64,

    ctx: SizeCtx,
    tables: [TableInfo; TABLE_COUNT],
}

impl<'a> Metadata<'a> {
    pub(crate) fn from_image(image: &PeImage<'a>) -> Result<Metadata<'a>> {
        let mut strings_data: &[u8] = &[];
        let mut blob_data: &[u8] = &[];
        let mut us_data: &[u8] = &[];
        let mut guid_data: &[u8] = &[];
        let mut tables_data: Option<&[u8]> = None;
        let mut pdb_data = None;

        for stream in &image.stream_headers {
            match stream.name {
                "#~" | "#-" => tables_data = Some(stream.data),
                "#Strings" => strings_data = stream.data,
                "#Blob" => blob_data = stream.data,
                "#GUID" => guid_data = stream.data,
                "#US" => us_data = stream.data,
                "#Pdb" => pdb_data = Some(stream.data),
                _ => {}
            }
        }

        let tables_data =
            tables_data.ok_or(Error::bad(image.metadata_offset, "no #~ or #- stream"))?;

        // ImageReader.ReadTableHeap
        let mut cur = Cur::new(tables_data);
        cur.skip(6)?; // Reserved, MajorVersion, MinorVersion
        let heap_sizes = cur.u8()?;
        cur.skip(1)?; // Reserved2
        let valid = cur.u64()?;
        let sorted = cur.u64()?;

        let mut row_counts = [0u32; TABLE_COUNT];
        for (i, count) in row_counts.iter_mut().enumerate() {
            if valid & (1u64 << i) != 0 {
                *count = cur.u32()?;
            }
        }

        let mut ctx = SizeCtx {
            heap_sizes,
            row_counts,
            coded_sizes: [0; CODED_INDEX_COUNT],
        };
        for i in 0..CODED_INDEX_COUNT {
            // Safety of the transmute-free approach: enumerate the kinds.
            let kind = CODED_INDEX_KINDS[i];
            ctx.coded_sizes[i] = kind.size(|t| ctx.row_counts[t as usize]);
        }

        // ComputeTableInformations: rows are laid out immediately after the
        // header, present tables in id order.
        let mut offset = cur.pos() as u32;
        let mut tables = [TableInfo::default(); TABLE_COUNT];
        for (i, info) in tables.iter_mut().enumerate() {
            if valid & (1u64 << i) == 0 {
                continue;
            }
            let table = Table::from_index(i)
                .ok_or(Error::bad(cur.pos(), "unknown table id in Valid bitvector"))?;
            let row_size = table_row_size(table, &ctx);
            *info = TableInfo {
                offset,
                rows: ctx.row_counts[i],
                row_size,
            };
            offset = offset
                .checked_add(row_size.wrapping_mul(ctx.row_counts[i]))
                .ok_or(Error::bad(offset as usize, "table data overflow"))?;
        }

        Ok(Metadata {
            strings_data,
            blob_data,
            us_data,
            guid_data,
            tables_data,
            pdb_data,
            heap_sizes,
            valid,
            sorted,
            ctx,
            tables,
        })
    }

    pub(crate) fn size_ctx(&self) -> &SizeCtx {
        &self.ctx
    }

    pub fn strings(&self) -> StringsHeap<'a> {
        StringsHeap {
            data: self.strings_data,
        }
    }

    pub fn blobs(&self) -> BlobHeap<'a> {
        BlobHeap {
            data: self.blob_data,
        }
    }

    pub fn user_strings(&self) -> UserStringsHeap<'a> {
        UserStringsHeap { data: self.us_data }
    }

    pub fn guids(&self) -> GuidHeap<'a> {
        GuidHeap {
            data: self.guid_data,
        }
    }

    pub fn has_table(&self, table: Table) -> bool {
        self.row_count(table) > 0
    }

    pub fn row_count(&self, table: Table) -> u32 {
        self.tables[table as usize].rows
    }

    pub fn table_info(&self, table: Table) -> TableInfo {
        self.tables[table as usize]
    }

    /// Typed accessor for a metadata table.
    pub fn table<R: RawRow>(&self) -> TableRef<'_, R> {
        TableRef::new(self, self.tables[R::TABLE as usize])
    }

    /// Reads the row with 1-based `rid` from `R`'s table.
    pub fn row<R: RawRow>(&self, rid: u32) -> Result<R> {
        self.table::<R>().row(rid)
    }

    pub fn is_sorted(&self, table: Table) -> bool {
        self.sorted & (1u64 << table as u8) != 0
    }
}

const CODED_INDEX_KINDS: [CodedIndexKind; CODED_INDEX_COUNT] = [
    CodedIndexKind::TypeDefOrRef,
    CodedIndexKind::HasConstant,
    CodedIndexKind::HasCustomAttribute,
    CodedIndexKind::HasFieldMarshal,
    CodedIndexKind::HasDeclSecurity,
    CodedIndexKind::MemberRefParent,
    CodedIndexKind::HasSemantics,
    CodedIndexKind::MethodDefOrRef,
    CodedIndexKind::MemberForwarded,
    CodedIndexKind::Implementation,
    CodedIndexKind::CustomAttributeType,
    CodedIndexKind::ResolutionScope,
    CodedIndexKind::TypeOrMethodDef,
    CodedIndexKind::HasCustomDebugInformation,
];
