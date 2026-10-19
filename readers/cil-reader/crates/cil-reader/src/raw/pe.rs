//! PE image parsing: DOS/COFF/optional headers, sections, CLI header,
//! metadata root, debug directory.
//!
//! Port of `Mono.Cecil.PE/ImageReader.cs` (read side).

use crate::error::{Error, Result};
use crate::raw::cursor::Cur;
use crate::raw::metadata::Metadata;
use std::borrow::Cow;

/// `Mono.Cecil.ModuleKind` (writes aside, `NetModule` is decided at the model
/// layer by the absence of an Assembly row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Dll,
    Console,
    Windows,
    NetModule,
}

/// `Mono.Cecil.TargetArchitecture`; unknown machine values are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArchitecture {
    I386,
    Amd64,
    Ia64,
    Arm,
    Armv7,
    Arm64,
    Unknown(u16),
}

impl TargetArchitecture {
    fn from_machine(machine: u16) -> TargetArchitecture {
        match machine {
            0x014c => TargetArchitecture::I386,
            0x8664 => TargetArchitecture::Amd64,
            0x0200 => TargetArchitecture::Ia64,
            0x01c0 => TargetArchitecture::Arm,
            0x01c4 => TargetArchitecture::Armv7,
            0xaa64 => TargetArchitecture::Arm64,
            other => TargetArchitecture::Unknown(other),
        }
    }
}

/// `Mono.Cecil.ModuleAttributes` (CLI header flags).
pub mod module_attributes {
    pub const IL_ONLY: u32 = 1;
    pub const REQUIRED_32_BIT: u32 = 2;
    pub const IL_LIBRARY: u32 = 4;
    pub const STRONG_NAME_SIGNED: u32 = 8;
    pub const PREFERRED_32_BIT: u32 = 0x0002_0000;
}

/// `Mono.Cecil.ModuleCharacteristics` (PE DllCharacteristics).
pub mod module_characteristics {
    pub const HIGH_ENTROPY_VA: u16 = 0x0020;
    pub const DYNAMIC_BASE: u16 = 0x0040;
    pub const NX_COMPAT: u16 = 0x0100;
    pub const NO_SEH: u16 = 0x0400;
    pub const APP_CONTAINER: u16 = 0x1000;
    pub const TERMINAL_SERVER_AWARE: u16 = 0x8000;
}

/// `Mono.Cecil.TargetRuntime`, derived from the metadata version string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRuntime {
    Net1_0,
    Net1_1,
    Net2_0,
    Net4_0,
}

/// `Mixin.ParseRuntime`: dispatch on the second character of "v1.1.4322"-style
/// version strings.
pub fn parse_runtime(version: &str) -> TargetRuntime {
    let bytes = version.as_bytes();
    match bytes.get(1) {
        Some(b'1') => {
            if bytes.get(3) == Some(&b'0') {
                TargetRuntime::Net1_0
            } else {
                TargetRuntime::Net1_1
            }
        }
        Some(b'2') => TargetRuntime::Net2_0,
        _ => TargetRuntime::Net4_0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

impl DataDirectory {
    pub fn is_zero(&self) -> bool {
        self.virtual_address == 0 && self.size == 0
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
}

// Cecil's Image does this arithmetic as unchecked uints, so a header whose
// sums pass 2^32 wraps rather than failing.
impl Section {
    /// Whether `rva` falls in the section's raw data
    /// (`Image.GetSectionAtVirtualAddress`).
    pub fn contains_rva(&self, rva: u32) -> bool {
        rva >= self.virtual_address
            && rva < self.virtual_address.wrapping_add(self.size_of_raw_data)
    }

    /// The file offset of `rva`, which the section contains
    /// (`Image.ResolveVirtualAddressInSection`).
    pub fn file_offset(&self, rva: u32) -> u32 {
        rva.wrapping_add(self.pointer_to_raw_data)
            .wrapping_sub(self.virtual_address)
    }
}

/// One entry of the PE debug directory (`Mono.Cecil.ImageDebugDirectory` +
/// its raw data).
#[derive(Debug, Clone)]
pub struct DebugHeaderEntry<'a> {
    pub characteristics: i32,
    pub time_date_stamp: u32,
    pub major_version: u16,
    pub minor_version: u16,
    /// `ImageDebugType`: 2 = CodeView, 16 = Deterministic, 17 = EmbeddedPortablePdb,
    /// 19 = PdbChecksum.
    pub debug_type: u32,
    pub size_of_data: u32,
    pub address_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub data: &'a [u8],
}

pub mod image_debug_type {
    pub const CODE_VIEW: u32 = 2;
    pub const DETERMINISTIC: u32 = 16;
    pub const EMBEDDED_PORTABLE_PDB: u32 = 17;
    pub const PDB_CHECKSUM: u32 = 19;
}

/// A parsed PE image with CLI metadata directories located.
pub struct PeImage<'a> {
    pub(crate) data: &'a [u8],
    pub kind: ModuleKind,
    pub characteristics: u16,
    pub architecture: TargetArchitecture,
    pub dll_characteristics: u16,
    pub linker_version: u16,
    pub subsystem_major: u16,
    pub subsystem_minor: u16,
    pub timestamp: u32,
    pub sections: Vec<Section>,
    /// CLI header flags (`ModuleAttributes`).
    pub attributes: u32,
    pub entry_point_token: u32,
    /// The metadata version string, as Latin-1.
    pub runtime_version: Cow<'a, str>,
    pub metadata_directory: DataDirectory,
    pub debug_directory: DataDirectory,
    pub resources_directory: DataDirectory,
    pub strong_name_directory: DataDirectory,
    pub win32_resources_directory: DataDirectory,
    pub debug_header: Vec<DebugHeaderEntry<'a>>,
    /// File offset of the metadata root (BSJB).
    pub(crate) metadata_offset: usize,
    pub(crate) stream_headers: Vec<StreamHeader<'a>>,
}

#[derive(Debug, Clone)]
pub(crate) struct StreamHeader<'a> {
    /// The name, as Latin-1.
    pub name: Cow<'a, str>,
    pub data: &'a [u8],
}

impl<'a> PeImage<'a> {
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Resolves an RVA to a file offset (Cecil `Image.ResolveVirtualAddress`).
    pub fn resolve_rva(&self, rva: u32) -> Result<usize> {
        let section = self
            .section_at_rva(rva)
            .ok_or(Error::bad(rva as usize, "rva outside any section"))?;
        Ok(section.file_offset(rva) as usize)
    }

    pub fn section_at_rva(&self, rva: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.contains_rva(rva))
    }

    /// Returns the `size`-byte slice starting at `rva`.
    pub fn slice_at_rva(&self, rva: u32, size: usize) -> Result<&'a [u8]> {
        let off = self.resolve_rva(rva)?;
        self.data
            .get(off..off + size)
            .ok_or(Error::bad(off, "data extends past end of file"))
    }

    pub fn parse(data: &'a [u8]) -> Result<PeImage<'a>> {
        // ImageReader.ReadImage
        if data.len() < 128 {
            return Err(Error::bad(0, "file too small to be a PE image"));
        }

        let mut cur = Cur::new(data);
        if cur.u16()? != 0x5a4d {
            return Err(Error::bad(0, "not an MZ executable"));
        }
        cur.skip(58)?;
        let lfanew = cur.u32()? as usize;
        cur.set_pos(lfanew);
        if cur.u32()? != 0x0000_4550 {
            return Err(Error::bad(lfanew, "missing PE signature"));
        }

        // COFF file header
        let architecture = TargetArchitecture::from_machine(cur.u16()?);
        let section_count = cur.u16()?;
        let timestamp = cur.u32()?;
        cur.skip(10)?;
        let characteristics = cur.u16()?;

        // Optional header
        let pe64 = cur.u16()? == 0x20b;
        let linker_version = cur.u16()?;
        cur.skip(44)?;
        let subsystem_major = cur.u16()?;
        let subsystem_minor = cur.u16()?;
        cur.skip(16)?;
        let subsystem = cur.u16()?;
        let dll_characteristics = cur.u16()?;
        cur.skip(if pe64 { 56 } else { 40 })?;
        let win32_resources_directory = read_data_directory(&mut cur)?;
        cur.skip(24)?;
        let debug_directory = read_data_directory(&mut cur)?;
        cur.skip(56)?;
        let cli = read_data_directory(&mut cur)?;
        if cli.is_zero() {
            return Err(Error::bad(cur.pos(), "no CLI header"));
        }
        cur.skip(8)?;

        // Section headers
        let mut sections = Vec::with_capacity(section_count as usize);
        for _ in 0..section_count {
            let name_bytes = cur.bytes(8)?;
            let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(8);
            let name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
            let virtual_size = cur.u32()?;
            let virtual_address = cur.u32()?;
            let size_of_raw_data = cur.u32()?;
            let pointer_to_raw_data = cur.u32()?;
            cur.skip(16)?;
            sections.push(Section {
                name,
                virtual_address,
                virtual_size,
                size_of_raw_data,
                pointer_to_raw_data,
            });
        }

        let mut image = PeImage {
            data,
            kind: module_kind(characteristics, subsystem),
            characteristics,
            architecture,
            dll_characteristics,
            linker_version,
            subsystem_major,
            subsystem_minor,
            timestamp,
            sections,
            attributes: 0,
            entry_point_token: 0,
            runtime_version: Cow::Borrowed(""),
            metadata_directory: DataDirectory::default(),
            debug_directory,
            resources_directory: DataDirectory::default(),
            strong_name_directory: DataDirectory::default(),
            win32_resources_directory,
            debug_header: Vec::new(),
            metadata_offset: 0,
            stream_headers: Vec::new(),
        };

        image.read_cli_header(cli)?;
        image.read_metadata_root()?;
        image.read_debug_header()?;
        Ok(image)
    }

    fn read_cli_header(&mut self, cli: DataDirectory) -> Result<()> {
        let off = self.resolve_rva(cli.virtual_address)?;
        let mut cur = Cur::at(self.data, off);
        cur.skip(8)?; // Cb, MajorRuntimeVersion, MinorRuntimeVersion
        self.metadata_directory = read_data_directory(&mut cur)?;
        self.attributes = cur.u32()?;
        self.entry_point_token = cur.u32()?;
        self.resources_directory = read_data_directory(&mut cur)?;
        self.strong_name_directory = read_data_directory(&mut cur)?;
        Ok(())
    }

    fn read_metadata_root(&mut self) -> Result<()> {
        let md_off = self.resolve_rva(self.metadata_directory.virtual_address)?;
        self.metadata_offset = md_off;
        let (runtime_version, stream_headers) = read_metadata_root(self.data, md_off)?;
        self.runtime_version = runtime_version;
        self.stream_headers = stream_headers;
        Ok(())
    }

    fn read_debug_header(&mut self) -> Result<()> {
        if self.debug_directory.is_zero() {
            return Ok(());
        }
        let off = self.resolve_rva(self.debug_directory.virtual_address)?;
        let mut cur = Cur::at(self.data, off);
        let count = self.debug_directory.size / 28;
        for _ in 0..count {
            let characteristics = cur.i32()?;
            let time_date_stamp = cur.u32()?;
            let major_version = cur.u16()?;
            let minor_version = cur.u16()?;
            let debug_type = cur.u32()?;
            let size_of_data = cur.u32()?;
            let address_of_raw_data = cur.u32()?;
            let pointer_to_raw_data = cur.u32()?;
            // ImageReader.ReadDebugHeader tolerates absent data.
            let data = if pointer_to_raw_data == 0 {
                &[][..]
            } else {
                self.data
                    .get(
                        pointer_to_raw_data as usize
                            ..pointer_to_raw_data as usize + size_of_data as usize,
                    )
                    .unwrap_or(&[])
            };
            self.debug_header.push(DebugHeaderEntry {
                characteristics,
                time_date_stamp,
                major_version,
                minor_version,
                debug_type,
                size_of_data,
                address_of_raw_data,
                pointer_to_raw_data,
                data,
            });
        }
        Ok(())
    }

    /// Locates the metadata streams and reads the table header.
    pub fn metadata(&self) -> Result<Metadata<'a>> {
        Metadata::from_image(self)
    }
}

/// Reads the metadata root (BSJB) at file offset `md_off`: the version
/// string, as Latin-1, and the stream headers. A PE image's root is where its
/// CLI header points; a portable PDB's is at offset 0.
pub(crate) fn read_metadata_root(
    data: &[u8],
    md_off: usize,
) -> Result<(Cow<'_, str>, Vec<StreamHeader<'_>>)> {
    let mut cur = Cur::at(data, md_off);
    if cur.u32()? != 0x424a_5342 {
        return Err(Error::bad(md_off, "missing BSJB metadata signature"));
    }
    cur.skip(8)?; // MajorVersion, MinorVersion, Reserved
    let version_len = cur.u32()? as usize;
    let version_bytes = cur.bytes(version_len)?;
    let end = version_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(version_len);
    let version = latin1(&version_bytes[..end]);
    cur.skip(2)?; // Flags
    let stream_count = cur.u16()?;

    let mut streams = Vec::with_capacity(stream_count.into());
    for _ in 0..stream_count {
        let at = cur.pos();
        // ImageReader.ReadMetadataStream: the offset is added to the
        // root's file offset in unchecked uint arithmetic.
        let start = (md_off as u32).wrapping_add(cur.u32()?) as usize;
        let size = cur.u32()?;
        let name = read_aligned_string(&mut cur, 16)?;
        // ReadHeapData reads with BinaryReader.ReadBytes((int) size),
        // which throws on a negative count and otherwise stops at the
        // end of the file.
        if size > i32::MAX as u32 {
            return Err(Error::bad(at, "metadata stream size past 2^31"));
        }
        let rest = data.get(start..).unwrap_or(&[]);
        let data = &rest[..(size as usize).min(rest.len())];
        streams.push(StreamHeader { name, data });
    }
    Ok((version, streams))
}

fn module_kind(characteristics: u16, subsystem: u16) -> ModuleKind {
    if characteristics & 0x2000 != 0 {
        ModuleKind::Dll
    } else if subsystem == 0x2 || subsystem == 0x9 {
        ModuleKind::Windows
    } else {
        ModuleKind::Console
    }
}

fn read_data_directory(cur: &mut Cur<'_>) -> Result<DataDirectory> {
    Ok(DataDirectory {
        virtual_address: cur.u32()?,
        size: cur.u32()?,
    })
}

/// `ImageReader.ReadAlignedString`: reads up to `max` bytes until NUL, then
/// aligns the cursor to the next 4-byte boundary.
fn read_aligned_string<'a>(cur: &mut Cur<'a>, max: usize) -> Result<Cow<'a, str>> {
    let start = cur.pos();
    let mut read = 0;
    while read < max {
        if cur.u8()? == 0 {
            break;
        }
        read += 1;
    }
    let consumed = cur.pos() - start;
    let aligned = (read + 4) & !3;
    cur.skip(aligned - consumed)?;
    let mut end = Cur::at(cur_data(cur), start);
    Ok(latin1(end.bytes(read)?))
}

/// Decodes `bytes` as Latin-1, as Cecil does by casting each byte to `char`.
fn latin1(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(s) if s.is_ascii() => Cow::Borrowed(s),
        _ => Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect()),
    }
}

// Small helper so read_aligned_string can re-slice the original buffer.
fn cur_data<'a>(cur: &Cur<'a>) -> &'a [u8] {
    cur.data()
}

#[cfg(test)]
mod tests {
    use super::Section;

    fn section(virtual_address: u32, size_of_raw_data: u32, pointer_to_raw_data: u32) -> Section {
        Section {
            name: ".text".into(),
            virtual_address,
            virtual_size: size_of_raw_data,
            size_of_raw_data,
            pointer_to_raw_data,
        }
    }

    // A section whose end passes 2^32 wraps, and so contains no RVA.
    #[test]
    fn section_end_wraps() {
        let s = section(0xffff_f000, 0x2000, 0x200);
        assert!(!s.contains_rva(0xffff_f000));
        assert!(!s.contains_rva(0x800));
        assert!(section(0xffff_f000, 0xfff, 0x200).contains_rva(0xffff_fffe));
    }

    // A file offset that passes 2^32 wraps.
    #[test]
    fn file_offset_wraps() {
        let s = section(0x2000, 0x1000, 0xffff_ff00);
        assert!(s.contains_rva(0x2100));
        assert_eq!(s.file_offset(0x2100), 0);
        assert_eq!(s.file_offset(0x2000), 0xffff_ff00);
    }
}
