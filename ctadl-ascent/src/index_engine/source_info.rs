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
use crate::facts::{FunctionId, IdMap, InsnId, InsnSiteId, PackedInsnSiteId};

/// Used to keep track of source info and instruction IDs during code generation.
#[derive(Default, Debug, Clone)]
pub struct IndexSourceInfo {
    /// Keeps track of mapping between function names and instruction sites
    pub sites: IdMap,
    pub insn_counter: InsnId,
    /// Maps instruction sites and source info
    pub source_map: HashMap<PackedInsnSiteId, FileSpanId>,
}

impl IndexSourceInfo {
    /// Allocates a fresh instruction ID and returns the instruction site representing the
    /// instruction and its containing function
    pub fn add_insn_site(&mut self, function_id: FunctionId) -> InsnSiteId {
        let insn_id = self.insn_counter;
        self.insn_counter.incr_assign();
        InsnSiteId::new(function_id, insn_id)
    }

    /// Associates the instruction site with a source span
    pub fn add_instruction_span(&mut self, site_id: PackedInsnSiteId, span_id: FileSpanId) {
        self.source_map.insert(site_id, span_id);
    }

    /// Saves the source info, including idmap, into parquet files.
    pub fn try_save<P: AsRef<std::path::Path>>(self, path: P) -> Result<(), Error> {
        use crate::facts::schema::*;
        let path = path.as_ref();
        self.sites.try_save(path)?;
        index_source_map::try_save(
            path,
            self.source_map.into_iter().map(|(site_id, span_id)| {
                let InsnSiteId { func_id, insn_id } = InsnSiteId::unpack(&site_id).unwrap();
                (func_id, insn_id, span_id)
            }),
        )?;
        Ok(())
    }
}
