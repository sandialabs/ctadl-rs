/*! Source info for index

The normal flow is that when we codegen the facts for indexing, we keep track of the original
source info for each instruction. We also have to keep track of a mapping between function names
and IDs, since the indexer uses IDs natively. This information is expensive and, ideally, would be
streamed out as it is generated. Instead, we compress it as much as possible in memory and then
save ot into parquet with [`IndexSourceInfo::try_save`]. *But*, there is no `try_load`, which is on
purpose. Since the source info is in parquet files, we can query them efficiently with datafusion,
which we do when formatting.
*/
use hashbrown::hash_map::HashMap;
use packed_struct::prelude::*;

use source_info::FileSpanId;

use crate::error::Error;
use crate::facts::{FunctionId, IdMap, ImportId, InsnId, InsnSiteId, PackedInsnSiteId};

/// Used to keep track of source info and instruction IDs during code generation.
#[derive(Default, Debug, Clone)]
pub struct IndexSourceInfo {
    /// Keeps track of mapping between function names and instruction sites
    pub sites: IdMap,
    pub insn_counter: InsnId,
    /// Maps instruction sites to source info: the span, and the import whose source-info
    /// database that span is an index into (see [`ImportId`]).
    pub source_map: HashMap<PackedInsnSiteId, (FileSpanId, ImportId)>,
    /// The imports codegen has run over, in order. The [`ImportId`] of a span is its position
    /// here, and [`Self::current_import`] is the one being codegen'd right now.
    imports: Vec<String>,
}

impl IndexSourceInfo {
    /// Declares that the spans recorded from here on belong to the import named `name`, and
    /// returns its id.
    ///
    /// Call it once per import, before codegen for that import runs. Spans recorded before any
    /// call belong to import 0, which is what a single-import index gets for free.
    pub fn begin_import(&mut self, name: &str) -> ImportId {
        self.imports.push(name.to_string());
        self.current_import()
    }

    /// The import codegen is currently running over. Zero before the first
    /// [`Self::begin_import`], which is the id a single-import index uses throughout.
    pub fn current_import(&self) -> ImportId {
        ImportId(self.imports.len().saturating_sub(1) as u32)
    }

    /// Allocates a fresh instruction ID and returns the instruction site representing the
    /// instruction and its containing function
    pub fn add_insn_site(&mut self, function_id: FunctionId) -> InsnSiteId {
        let insn_id = self.insn_counter;
        self.insn_counter.incr_assign();
        InsnSiteId::new(function_id, insn_id)
    }

    /// Associates the instruction site with a source span in the import being codegen'd.
    pub fn add_instruction_span(&mut self, site_id: PackedInsnSiteId, span_id: FileSpanId) {
        let import = self.current_import();
        self.source_map.insert(site_id, (span_id, import));
    }

    /// Saves the source info, including idmap, into parquet files.
    pub fn try_save<P: AsRef<std::path::Path>>(self, path: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        let path = path.as_ref();
        // Spans with no import named for them resolve nowhere: the formatter looks each one up
        // in the database of the import that numbered it, and there is none. Say so here rather
        // than let a caller that forgot [`Self::begin_import`] produce a log with no locations
        // in it and no reason given.
        if !self.source_map.is_empty() && self.imports.is_empty() {
            log::warn!(
                "saving {} source spans with no import recorded for them; \
                 results will have no source locations (this is a bug: \
                 `IndexSourceInfo::begin_import` was never called)",
                self.source_map.len()
            );
        }
        self.sites.try_save(path)?;
        // The names the ids stand for, so the formatter can resolve each span against the
        // source-info database it was numbered in.
        import_id::try_save(
            path,
            self.imports
                .iter()
                .enumerate()
                .map(|(i, name)| (ImportId(i as u32), name.clone())),
        )?;
        index_source_map::try_save(
            path,
            self.source_map
                .into_iter()
                .map(|(site_id, (span_id, import_id))| {
                    let InsnSiteId { func_id, insn_id } = InsnSiteId::unpack(&site_id).unwrap();
                    (func_id, insn_id, span_id, import_id)
                }),
        )?;
        Ok(())
    }
}
