//! Portable PDB: a module's documents and sequence points.
//!
//! A portable PDB is metadata with no PE around it: a root at offset 0, a
//! `#Pdb` stream, and the debug tables, which index the tables of the
//! assembly it describes. [`PortablePdb`] reads the parts the frontend maps
//! IL offsets to source lines with, as Cecil's `MetadataReader` reads them
//! (`InitializeDocuments`, `ReadSequencePoints`, `ReadDocumentName`).
//!
//! [`PdbSource::locate`] finds a module's PDB as Cecil's
//! `DefaultSymbolReaderProvider` does: debug tables in the module itself,
//! else an embedded PDB, else a portable `.pdb` beside the file. Native
//! (Windows) PDBs and `.mdb` files are not read.

use std::borrow::Cow;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::module::Module;
use crate::raw::cursor::Cur;
use crate::raw::heaps::{BlobIndex, Guid};
use crate::raw::metadata::Metadata;
use crate::raw::pe::{image_debug_type, read_metadata_root};
use crate::raw::tables::{DocumentRaw, MethodDebugInformationRaw, Table};
use crate::token::{MetadataToken, TokenType};

/// The line of a hidden sequence point: code that maps to no source line.
pub const HIDDEN_LINE: i32 = 0xfeefee;

/// The tables whose presence makes an image hold its own debug information
/// (Cecil `Image.HasDebugTables`).
const DEBUG_TABLES: [Table; 7] = [
    Table::Document,
    Table::MethodDebugInformation,
    Table::LocalScope,
    Table::LocalVariable,
    Table::LocalConstant,
    Table::StateMachineMethod,
    Table::CustomDebugInformation,
];

/// A module's portable PDB, read.
pub struct PortablePdb<'a> {
    md: Metadata<'a>,
}

/// A source document (Cecil `Document`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document<'a> {
    pub token: MetadataToken,
    /// The path, as the compiler wrote it.
    pub name: String,
    /// `HashAlgorithmGuid`: SHA-1 or SHA-256 (see [`hash_algorithm`]), or
    /// zero when there is no hash.
    pub hash_algorithm: Guid,
    pub hash: &'a [u8],
    pub language: Guid,
}

/// The GUIDs a document's `hash_algorithm` takes.
pub mod hash_algorithm {
    use crate::raw::heaps::Guid;

    /// `{ff1816ec-aa5e-4d10-87f7-6f4963833460}`
    pub const SHA1: Guid = Guid([
        0xec, 0x16, 0x18, 0xff, 0x5e, 0xaa, 0x10, 0x4d, 0x87, 0xf7, 0x6f, 0x49, 0x63, 0x83, 0x34,
        0x60,
    ]);
    /// `{8829d00f-11b8-4213-878b-770e8597ac16}`
    pub const SHA256: Guid = Guid([
        0x0f, 0xd0, 0x29, 0x88, 0xb8, 0x11, 0x13, 0x42, 0x87, 0x8b, 0x77, 0x0e, 0x85, 0x97, 0xac,
        0x16,
    ]);
}

/// A sequence point (Cecil `SequencePoint`): the IL at `offset`, up to the
/// next point, is the source between the start and the end. Lines and
/// columns are 1-based; columns count UTF-16 code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequencePoint {
    pub offset: u32,
    /// The document's rid.
    pub document: u32,
    pub start_line: i32,
    pub start_column: i32,
    pub end_line: i32,
    pub end_column: i32,
}

impl SequencePoint {
    /// Whether the point maps its IL to no source ([`HIDDEN_LINE`]).
    pub fn is_hidden(&self) -> bool {
        self.start_line == HIDDEN_LINE
    }
}

impl<'a> PortablePdb<'a> {
    /// Reads a standalone portable PDB (a `.pdb` file, or an embedded one
    /// decompressed), as Cecil's `ImageReader.ReadPortablePdb` does.
    pub fn read(data: &'a [u8]) -> Result<PortablePdb<'a>> {
        let (_, streams) = read_metadata_root(data, 0)?;
        let md = Metadata::from_streams(&streams, 0)?;
        if md.pdb.is_none() {
            return Err(Error::bad(0, "portable PDB with no #Pdb stream"));
        }
        Ok(PortablePdb { md })
    }

    /// The debug tables `module` holds itself, if it has any.
    pub fn in_module(module: &Module<'a>) -> Result<Option<PortablePdb<'a>>> {
        let md = module.image().metadata()?;
        if !DEBUG_TABLES.iter().any(|&t| md.is_valid(t)) {
            return Ok(None);
        }
        Ok(Some(PortablePdb { md }))
    }

    /// The PDB id from `#Pdb`; `None` for debug tables in a module.
    pub fn id(&self) -> Option<[u8; 20]> {
        self.md.pdb.map(|p| p.id)
    }

    /// Whether the PDB describes `module`: some CodeView entry of its debug
    /// directory names the PDB id's GUID (Cecil `IsMatchingEntry`). Debug
    /// tables in a module describe it.
    pub fn matches(&self, module: &Module<'_>) -> bool {
        let Some(id) = self.id() else {
            return true;
        };
        module.image().debug_header.iter().any(|entry| {
            entry.debug_type == image_debug_type::CODE_VIEW
                && entry.data.len() >= 24
                && entry.data[..4] == *b"RSDS"
                && entry.data[4..20] == id[..16]
        })
    }

    /// The number of rows of the Document table.
    pub fn document_count(&self) -> u32 {
        self.md.row_count(Table::Document)
    }

    /// Every document, by rid (Cecil `InitializeDocuments`).
    pub fn documents(&self) -> Result<Vec<Document<'a>>> {
        (1..=self.document_count())
            .map(|rid| self.document(rid))
            .collect()
    }

    /// The document with `rid`, which must be a row of the Document table.
    pub fn document(&self, rid: u32) -> Result<Document<'a>> {
        let row: DocumentRaw = self.md.row(rid)?;
        let guids = self.md.guids();
        Ok(Document {
            token: MetadataToken::new(TokenType::Document, rid),
            name: self.document_name(row.name)?,
            hash_algorithm: guids.get(row.hash_algorithm)?,
            hash: self.md.blobs().get(row.hash)?,
            language: guids.get(row.language)?,
        })
    }

    /// `SignatureReader.ReadDocumentName`: a separator character, then
    /// blob indices of the UTF-8 parts it joins.
    fn document_name(&self, index: BlobIndex) -> Result<String> {
        let blobs = self.md.blobs();
        let blob = blobs.view(index)?;
        let Some((&separator, _)) = blob.split_first() else {
            return Ok(String::new());
        };
        let mut cur = Cur::at(blob, 1);
        let mut name = String::new();
        let mut first = true;
        while cur.remaining() > 0 {
            if !first && separator != 0 {
                name.push(char::from(separator));
            }
            first = false;
            let part = cur.compressed_u32()?;
            if part != 0 {
                name.push_str(&String::from_utf8_lossy(blobs.view(BlobIndex(part))?));
            }
        }
        Ok(name)
    }

    /// The sequence points of the method with MethodDef `rid`, in IL order;
    /// none for a method the PDB has no row or blob for. An error where a
    /// point names no document, where Cecil throws.
    pub fn sequence_points(&self, method_rid: u32) -> Result<Vec<SequencePoint>> {
        if method_rid == 0 || method_rid > self.md.row_count(Table::MethodDebugInformation) {
            return Ok(Vec::new());
        }
        let row: MethodDebugInformationRaw = self.md.row(method_rid)?;
        if row.sequence_points.0 == 0 {
            return Ok(Vec::new());
        }
        let blob = self.md.blobs().view(row.sequence_points)?;
        let documents = self.document_count();
        let known = |rid: u32| (1..=documents).contains(&rid);
        let mut cur = Cur::new(blob);
        cur.compressed_u32()?; // LocalSignature

        // The row's document, else one the blob names first.
        let mut document = row.document;
        if !known(document) {
            document = cur.compressed_u32()?;
        }
        let mut points = Vec::with_capacity(blob.len() / 5);
        let mut offset = 0u32;
        let mut start_line = 0i32;
        let mut start_column = 0i32;
        let mut first_non_hidden = true;
        let mut i = 0usize;
        while cur.remaining() > 0 {
            let delta_il = cur.compressed_u32()?;
            if i > 0 && delta_il == 0 {
                document = cur.compressed_u32()?;
                i += 1;
                continue;
            }
            i += 1;
            offset = offset.wrapping_add(delta_il);
            let delta_lines = cur.compressed_u32()? as i32;
            let delta_columns = if delta_lines == 0 {
                cur.compressed_u32()? as i32
            } else {
                cur.compressed_i32()?
            };
            if !known(document) {
                return Err(Error::bad(cur.pos(), "sequence point in no document"));
            }
            if delta_lines == 0 && delta_columns == 0 {
                points.push(SequencePoint {
                    offset,
                    document,
                    start_line: HIDDEN_LINE,
                    start_column: 0,
                    end_line: HIDDEN_LINE,
                    end_column: 0,
                });
                continue;
            }
            if first_non_hidden {
                start_line = cur.compressed_u32()? as i32;
                start_column = cur.compressed_u32()? as i32;
            } else {
                start_line = start_line.wrapping_add(cur.compressed_i32()?);
                start_column = start_column.wrapping_add(cur.compressed_i32()?);
            }
            points.push(SequencePoint {
                offset,
                document,
                start_line,
                start_column,
                end_line: start_line.wrapping_add(delta_lines),
                end_column: start_column.wrapping_add(delta_columns),
            });
            first_non_hidden = false;
        }
        Ok(points)
    }
}

/// Where a module's portable PDB is, and its bytes.
pub enum PdbSource {
    /// The module holds its own debug tables.
    InModule,
    /// An `EmbeddedPortablePdb` debug entry, decompressed.
    Embedded(Vec<u8>),
    /// A portable `.pdb` beside the module's file.
    File(PathBuf, Vec<u8>),
}

impl PdbSource {
    /// Finds the PDB of `module`, read from `path`, as Cecil's
    /// `DefaultSymbolReaderProvider` does; `None` when there is no portable
    /// one. Only an I/O error reading a `.pdb` that exists, or an embedded
    /// PDB that does not decompress, is an error.
    pub fn locate(module: &Module<'_>, path: &Path) -> Result<Option<PdbSource>> {
        let md = module.image().metadata()?;
        if DEBUG_TABLES.iter().any(|&t| md.is_valid(t)) {
            return Ok(Some(PdbSource::InModule));
        }
        let embedded = module
            .image()
            .debug_header
            .iter()
            .find(|e| e.debug_type == image_debug_type::EMBEDDED_PORTABLE_PDB);
        if let Some(entry) = embedded {
            return Ok(Some(PdbSource::Embedded(inflate_embedded(entry.data)?)));
        }
        let pdb = path.with_extension("pdb");
        let Ok(data) = std::fs::read(&pdb) else {
            return Ok(None);
        };
        if data.get(..4) != Some(b"BSJB") {
            return Ok(None);
        }
        Ok(Some(PdbSource::File(pdb, data)))
    }

    /// Reads the PDB, which must describe `module` (Cecil's
    /// `ThrowIfSymbolsAreNotMatching`).
    pub fn read<'s>(&'s self, module: &Module<'s>) -> Result<PortablePdb<'s>> {
        let pdb = match self {
            PdbSource::InModule => {
                return PortablePdb::in_module(module)?
                    .ok_or(Error::bad(0, "the module holds no debug tables"));
            }
            PdbSource::Embedded(data) | PdbSource::File(_, data) => PortablePdb::read(data)?,
        };
        if !pdb.matches(module) {
            return Err(Error::bad(
                0,
                "symbols were found but do not match the assembly",
            ));
        }
        Ok(pdb)
    }

    /// How the PDB was found, for messages: `module`, `embedded`, or the
    /// `.pdb` path.
    pub fn describe(&self) -> Cow<'_, str> {
        match self {
            PdbSource::InModule => Cow::Borrowed("module"),
            PdbSource::Embedded(_) => Cow::Borrowed("embedded"),
            PdbSource::File(path, _) => path.to_string_lossy(),
        }
    }
}

/// `EmbeddedPortablePdbReaderProvider.GetPortablePdbStream`: a signature
/// (`MPDB`, unchecked), the decompressed size, then raw deflate.
fn inflate_embedded(data: &[u8]) -> Result<Vec<u8>> {
    let mut cur = Cur::new(data);
    cur.u32()?;
    let size = cur.u32()?;
    let mut out = Vec::with_capacity((size as usize).min(1 << 26));
    flate2::read::DeflateDecoder::new(&data[8..])
        .read_to_end(&mut out)
        .map_err(|_| Error::bad(8, "embedded PDB does not decompress"))?;
    Ok(out)
}
