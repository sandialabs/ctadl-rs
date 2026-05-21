//! Pcode reader library
//!
//! This crate provides functionality for reading and parsing Ghidra pcode facts.
//! It's designed to be used by the CTADL pcode frontend to convert pcode facts
//! into CTADL IR.

use datafusion::arrow::array::{Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::prelude::*;
use std::fmt::Display;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use internment::ArcIntern;
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet};

pub use error::PcodeError;

pub mod constant_propagation;
pub mod error;

/// Macro to generate newtype wrappers around ArcIntern<str>
/// Each newtype gets Deref and Display implementations
macro_rules! define_interned_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Deserialize)]
        #[repr(transparent)]
        pub struct $name(ArcIntern<str>);

        impl <S: AsRef<str>> From<S> for $name {
            fn from(s: S) -> Self {
                $name(s.as_ref().into())
            }
        }

        impl Deref for $name {
            type Target = ArcIntern<str>;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", &self.0)
            }
        }
    };
}

/// Re-export commonly used types
pub type Result<T> = std::result::Result<T, PcodeError>;

// Define all the interned newtypes using the macro
define_interned_newtype! {
    /// Unique identifier for a high-level function
    HighFunc
}

define_interned_newtype! {
    /// Function prototype/signature
    HighProto
}

define_interned_newtype! {
    /// Unique identifier for a pcode instruction
    PcodeInstruction
}

define_interned_newtype! {
    /// Unique identifier for a varnode (variable node)
    PcodeVarnode
}

define_interned_newtype! {
    /// Unique identifier for a basic block
    PcodeBlockBasic
}

define_interned_newtype! {
    /// Pcode instruction mnemonic (operation name)
    PcodeMnemonic
}

define_interned_newtype! {
    /// Pcode high symbol
    HighSymbol
}

define_interned_newtype! {
    /// Pcode high variable
    HighVariable
}

/// Pcode instruction target address (i64)
#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Deserialize)]
#[repr(transparent)]
pub struct PcodeAddress(pub i64);

impl Display for PcodeAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Pcode function data
#[derive(Debug, Clone)]
pub struct HFuncData {
    pub name: String,
    pub proto: Option<HighProto>,
    pub is_external: bool,
    pub entry_point: Option<PcodeAddress>,
    pub local_entry_points: Vec<PcodeAddress>,
}

/// Prototype data
#[derive(Debug, Clone)]
pub struct ProtoData {
    pub name: String,
    pub return_type: Option<String>,
    pub is_vararg: bool,
    pub is_void: bool,
    pub parameter_count: usize,
    pub parameters: Vec<ProtoParameter>,
}

/// Prototype parameter data
#[derive(Debug, Clone)]
pub struct ProtoParameter {
    pub symbol: HighSymbol,
}

/// Pcode instruction data
#[derive(Debug, Clone)]
pub struct PcodeData {
    pub mnemonic: PcodeMnemonic,
    pub opcode: Option<String>,
    pub inputs: SmallVec<[PcodeVarnode; 2]>,
    pub outputs: SmallVec<[PcodeVarnode; 1]>,
    pub bb_id: Option<PcodeBlockBasic>,
    pub index: i64,
    pub target: Option<PcodeAddress>,
}

/// Varnode data
#[derive(Debug, Clone)]
pub struct VnodeData {
    pub name: String,
    pub size: Option<i64>,
    pub is_address: bool,
    pub space: Option<String>,
    pub address: Option<PcodeAddress>,
    pub constant_offset: Option<i64>,
}

/// Register data
#[derive(Debug, Clone)]
pub struct RegisterData {
    pub offset: i64,
    pub size: i64,
    pub name: String,
    pub is_stack_pointer: bool,
}

/// Basic block data
#[derive(Debug, Clone)]
pub struct BBData {
    pub hfunc: HighFunc,
    pub start_address: Option<PcodeAddress>,
    pub first_inst: Option<PcodeInstruction>,
    pub last_inst: Option<PcodeInstruction>,
    pub instruction_indices: Vec<(u32, PcodeInstruction)>, // (index, instruction) pairs
    pub out_edges: Vec<PcodeBlockBasic>,
    pub tout_edges: Vec<PcodeBlockBasic>,
    pub fout_edges: Vec<PcodeBlockBasic>,
}

/// Pcode facts reader
#[derive(Debug)]
pub struct PcodeFactsReader {
    facts_dir: std::path::PathBuf,
}

impl PcodeFactsReader {
    /// Create a new pcode facts reader
    pub fn new<P: AsRef<Path>>(facts_dir: P) -> Self {
        Self {
            facts_dir: facts_dir.as_ref().to_path_buf(),
        }
    }

    /// Read all essential pcode facts
    pub fn read_all_facts(&self) -> Result<PcodeFacts> {
        log::trace!("pcode: reading hfunc facts");
        let hfunc_facts = self.read_hfunc_facts()?;
        log::trace!("pcode: reading pcode facts");
        let pcode_facts = self.read_pcode_facts()?;
        log::trace!("pcode: reading vnode facts");
        let vnode_facts = self.read_vnode_facts()?;
        log::trace!("pcode: reading bb facts");
        let bb_facts = self.read_bb_facts()?;
        log::trace!("pcode: reading proto facts");
        let proto_facts = self.read_proto_facts()?;
        log::trace!("pcode: reading symbol hvar facts");
        let symbol_hvar_facts = self.read_symbol_hvar_facts()?;
        log::trace!("pcode: reading hvar name facts");
        let hvar_name_facts = self.read_hvar_name_facts()?;
        log::trace!("pcode: reading hvar representative facts");
        let hvar_representative_facts = self.read_hvar_representative_facts()?;
        log::trace!("pcode: reading register facts");
        let register_facts = self.read_register_facts()?;

        Ok(PcodeFacts {
            hfunc_facts,
            pcode_facts,
            vnode_facts,
            bb_facts,
            proto_facts,
            symbol_hvar_facts,
            hvar_name_facts,
            hvar_representative_facts,
            register_facts,
        })
    }

    /// Read Register facts
    pub fn read_register_facts(&self) -> Result<Vec<RegisterData>> {
        let off_name_facts = self
            .read_csv_facts_optional::<(i64, i64, String)>("REGISTER_OFF_NAME.facts")?
            .unwrap_or_default();
        let is_sp_facts = self
            .read_csv_facts_optional::<String>("REGISTER_IS_SP.facts")?
            .unwrap_or_default();
        let sp_set: BTreeSet<String> = is_sp_facts.into_iter().collect();

        let mut result = Vec::new();
        for (offset, size, name) in off_name_facts {
            let is_stack_pointer = sp_set.contains(&name);
            result.push(RegisterData {
                offset,
                size,
                name,
                is_stack_pointer,
            });
        }
        Ok(result)
    }

    /// Read HFUNC (function) facts
    pub fn read_hfunc_facts(&self) -> Result<BTreeMap<HighFunc, HFuncData>> {
        let mut result = BTreeMap::new();

        // Read HFUNC_NAME.facts
        let name_facts = self.read_csv_facts::<(HighFunc, String)>("HFUNC_NAME.facts")?;

        // Read HFUNC_PROTO.facts
        let proto_facts = self.read_csv_facts::<(HighFunc, HighProto)>("HFUNC_PROTO.facts")?;

        // Read HFUNC_ISEXT.facts
        let isext_facts = self.read_csv_facts::<HighFunc>("HFUNC_ISEXT.facts")?;

        // Convert to more usable formats
        let mut name_map: BTreeMap<HighFunc, String> = BTreeMap::new();
        for (func_id, name) in name_facts {
            name_map.insert(func_id, name);
        }

        let mut proto_map: BTreeMap<HighFunc, HighProto> = BTreeMap::new();
        for (func_id, proto) in proto_facts {
            proto_map.insert(func_id, proto);
        }

        let isext_set: BTreeSet<HighFunc> = isext_facts.into_iter().collect();

        // Read HFUNC_EP.facts
        let ep_facts = self.read_csv_facts::<(HighFunc, i64)>("HFUNC_EP.facts")?;

        let mut ep_map: BTreeMap<HighFunc, PcodeAddress> = BTreeMap::new();
        let mut ep_to_func_map: BTreeMap<PcodeAddress, HighFunc> = BTreeMap::new();
        for (func_id, ep_address_val) in ep_facts {
            let ep_address = PcodeAddress(ep_address_val);
            ep_map.insert(func_id.clone(), ep_address.clone());
            ep_to_func_map.insert(ep_address, func_id);
        }

        // Read HFUNC_LOCAL_EP.facts
        let local_ep_facts = self.read_csv_facts::<(i64, i64)>("HFUNC_LOCAL_EP.facts")?;

        let mut local_ep_map: BTreeMap<HighFunc, Vec<PcodeAddress>> = BTreeMap::new();
        for (ep_val, local_ep_val) in local_ep_facts {
            let ep = PcodeAddress(ep_val);
            let local_ep = PcodeAddress(local_ep_val);
            // Find the function ID that has this entry point using the reverse map
            if let Some(func_id) = ep_to_func_map.get(&ep) {
                local_ep_map
                    .entry(func_id.clone())
                    .or_default()
                    .push(local_ep);
            }
        }

        // Combine the facts
        for (func_id, name) in name_map {
            let proto = proto_map.get(&func_id).cloned();
            let is_external = isext_set.contains(&func_id);
            let entry_point = ep_map.get(&func_id).cloned();
            let local_entry_points = local_ep_map.get(&func_id).cloned().unwrap_or_default();

            result.insert(
                func_id,
                HFuncData {
                    name,
                    proto,
                    is_external,
                    entry_point,
                    local_entry_points,
                },
            );
        }

        Ok(result)
    }

    /// Read PROTO (prototype) facts
    pub fn read_proto_facts(&self) -> Result<BTreeMap<HighProto, ProtoData>> {
        let mut result = BTreeMap::new();

        // Read PROTO_PARAMETER_COUNT.facts
        let param_count_facts =
            self.read_csv_facts::<(HighProto, usize)>("PROTO_PARAMETER_COUNT.facts")?;

        // Read PROTO_PARAMETER.facts
        let param_facts =
            self.read_csv_facts::<(HighProto, usize, HighSymbol)>("PROTO_PARAMETER.facts")?;

        let mut param_count_map: BTreeMap<HighProto, usize> = BTreeMap::new();
        for (proto_id, count) in param_count_facts {
            param_count_map.insert(proto_id, count);
        }

        let mut param_map: BTreeMap<HighProto, Vec<(usize, HighSymbol)>> = BTreeMap::new();
        for (proto_id, index, symbol) in param_facts {
            param_map.entry(proto_id).or_default().push((index, symbol));
        }

        // Read PROTO_RETTYPE.facts
        let rettype_facts = self.read_csv_facts::<(HighProto, String)>("PROTO_RETTYPE.facts")?;

        let mut rettype_map: BTreeMap<HighProto, String> = BTreeMap::new();
        for (proto_id, rettype) in rettype_facts {
            rettype_map.insert(proto_id, rettype);
        }

        // Read PROTO_IS_VARARG.facts
        let vararg_facts = self.read_csv_facts::<HighProto>("PROTO_IS_VARARG.facts")?;

        // Read PROTO_IS_VOID.facts
        let void_facts = self.read_csv_facts::<HighProto>("PROTO_IS_VOID.facts")?;

        let vararg_set: BTreeSet<HighProto> = vararg_facts.into_iter().collect();
        let void_set: BTreeSet<HighProto> = void_facts.into_iter().collect();

        // Combine the facts
        for (proto_id, param_count) in param_count_map {
            let parameters = self.build_parameters(&proto_id, param_count, &param_map);

            let return_type = rettype_map.get(&proto_id).cloned();
            let is_vararg = vararg_set.contains(&proto_id);
            let is_void = void_set.contains(&proto_id) || return_type.as_deref() == Some("void");

            result.insert(
                proto_id.clone(),
                ProtoData {
                    name: proto_id.to_string(),
                    return_type,
                    is_vararg,
                    is_void,
                    parameter_count: param_count,
                    parameters,
                },
            );
        }

        Ok(result)
    }

    /// Build parameters for a prototype
    fn build_parameters(
        &self,
        proto_id: &HighProto,
        param_count: usize,
        param_map: &BTreeMap<HighProto, Vec<(usize, HighSymbol)>>,
    ) -> Vec<ProtoParameter> {
        let mut parameters = Vec::with_capacity(param_count);

        // Get parameter names and indices
        let param_names = param_map.get(proto_id).cloned().unwrap_or_default();

        // Sort parameters by index
        let mut sorted_params: Vec<_> = param_names
            .iter()
            .map(|(idx, name)| (*idx, name.clone()))
            .collect();
        sorted_params.sort_by_key(|&(idx, _)| idx);

        // Build parameters
        for (_, symbol) in sorted_params {
            parameters.push(ProtoParameter { symbol });
        }

        parameters
    }

    /// Read PCODE facts
    pub fn read_pcode_facts(&self) -> Result<BTreeMap<PcodeInstruction, PcodeData>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async { self.read_pcode_facts_async().await })
    }

    async fn read_pcode_facts_async(&self) -> Result<BTreeMap<PcodeInstruction, PcodeData>> {
        let ctx = SessionContext::new();

        // Register all pcode related tables
        let tables = [
            (
                "PCODE_MNEMONIC.facts",
                "pcode_mnemonic",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "PCODE_OPCODE.facts",
                "pcode_opcode",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "PCODE_INPUT.facts",
                "pcode_input",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                    Field::new("c2", DataType::Utf8, false),
                ],
            ),
            (
                "PCODE_OUTPUT.facts",
                "pcode_output",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "PCODE_INDEX.facts",
                "pcode_index",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                ],
            ),
            (
                "PCODE_TARGET.facts",
                "pcode_target",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                ],
            ),
            (
                "PCODE_PARENT.facts",
                "pcode_parent",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "BB_HFUNC.facts",
                "bb_hfunc",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
        ];

        for (file, table, fields) in tables {
            let path = self.facts_dir.join(file);
            if path.exists() {
                let schema = Arc::new(Schema::new(fields));
                ctx.register_csv(
                    table,
                    path.to_str()
                        .ok_or_else(|| PcodeError::fact_consistency_error("Invalid path"))?,
                    CsvReadOptions::new()
                        .has_header(false)
                        .delimiter(b'\t')
                        .file_extension(".facts")
                        .schema(&schema),
                )
                .await?;
            }
        }

        // Build mnemonic map
        let mut mnemonic_map = BTreeMap::new();
        if ctx.table_exist("pcode_mnemonic")? {
            let df = ctx.sql("SELECT c0, c1 FROM pcode_mnemonic").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let c1 = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    mnemonic_map.insert(
                        PcodeInstruction::from(c0.value(i)),
                        PcodeMnemonic::from(c1.value(i)),
                    );
                }
            }
        }

        // Build opcode map
        let mut opcodes_by_inst = BTreeMap::new();
        if ctx.table_exist("pcode_opcode")? {
            let df = ctx.sql("SELECT c0, c1 FROM pcode_opcode").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let c1 = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    opcodes_by_inst
                        .insert(PcodeInstruction::from(c0.value(i)), c1.value(i).to_string());
                }
            }
        }

        // Build input map
        let mut inputs_by_inst: BTreeMap<PcodeInstruction, SmallVec<[PcodeVarnode; 2]>> =
            BTreeMap::new();
        if ctx.table_exist("pcode_input")? {
            let df = ctx
                .sql("SELECT c0, c1, c2 FROM pcode_input ORDER BY c0, c1")
                .await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                // Column 1 is index, but we ordered by it, so we can just push
                let c2 = batch
                    .column(2)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    inputs_by_inst
                        .entry(PcodeInstruction::from(c0.value(i)))
                        .or_default()
                        .push(PcodeVarnode::from(c2.value(i)));
                }
            }
        }

        // Build output map
        let mut outputs_by_inst: BTreeMap<PcodeInstruction, SmallVec<[PcodeVarnode; 1]>> =
            BTreeMap::new();
        if ctx.table_exist("pcode_output")? {
            let df = ctx.sql("SELECT c0, c1 FROM pcode_output").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let c1 = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    outputs_by_inst
                        .entry(PcodeInstruction::from(c0.value(i)))
                        .or_default()
                        .push(PcodeVarnode::from(c1.value(i)));
                }
            }
        }

        // Build index map
        let mut indices_by_inst = BTreeMap::new();
        if ctx.table_exist("pcode_index")? {
            let df = ctx.sql("SELECT c0, c1 FROM pcode_index").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let c1 = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    indices_by_inst.insert(PcodeInstruction::from(c0.value(i)), c1.value(i));
                }
            }
        }

        // Build target map
        let mut targets_by_inst = BTreeMap::new();
        if ctx.table_exist("pcode_target")? {
            let df = ctx.sql("SELECT c0, c1 FROM pcode_target").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let c1 = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    targets_by_inst.insert(
                        PcodeInstruction::from(c0.value(i)),
                        PcodeAddress(c1.value(i)),
                    );
                }
            }
        }

        // Build parent map
        let mut parent_map = BTreeMap::new();
        if ctx.table_exist("pcode_parent")? {
            let df = ctx.sql("SELECT c0, c1 FROM pcode_parent").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                let c1 = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    parent_map.insert(
                        PcodeInstruction::from(c0.value(i)),
                        PcodeBlockBasic::from(c1.value(i)),
                    );
                }
            }
        }

        // Build bb_ids set
        let mut bb_ids = BTreeSet::new();
        if ctx.table_exist("bb_hfunc")? {
            let df = ctx.sql("SELECT DISTINCT c0 FROM bb_hfunc").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                for i in 0..batch.num_rows() {
                    bb_ids.insert(PcodeBlockBasic::from(c0.value(i)));
                }
            }
        }

        let mut result = BTreeMap::new();
        for (inst_id, mnemonic) in mnemonic_map {
            let opcode = opcodes_by_inst.get(&inst_id).cloned();
            let inputs = inputs_by_inst.get(&inst_id).cloned().unwrap_or_default();
            let outputs = outputs_by_inst.get(&inst_id).cloned().unwrap_or_default();
            let index = indices_by_inst.get(&inst_id).copied().unwrap_or(0);
            let target = targets_by_inst.get(&inst_id).cloned();
            let bb_id = parent_map.get(&inst_id).and_then(|id| {
                if bb_ids.contains(id) {
                    Some(id.clone())
                } else {
                    None
                }
            });

            result.insert(
                inst_id,
                PcodeData {
                    mnemonic,
                    opcode,
                    inputs,
                    outputs,
                    bb_id,
                    index,
                    target,
                },
            );
        }

        Ok(result)
    }

    /// Read VNODE facts
    pub fn read_vnode_facts(&self) -> Result<BTreeMap<PcodeVarnode, VnodeData>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async { self.read_vnode_facts_async().await })
    }

    async fn read_vnode_facts_async(&self) -> Result<BTreeMap<PcodeVarnode, VnodeData>> {
        let ctx = SessionContext::new();

        let tables = [
            (
                "VNODE_NAME.facts",
                "vnode_name",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "VNODE_SIZE.facts",
                "vnode_size",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                ],
            ),
            (
                "VNODE_IS_ADDRESS.facts",
                "vnode_is_address",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                ],
            ),
            (
                "VNODE_SPACE.facts",
                "vnode_space",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "VNODE_ADDRESS.facts",
                "vnode_address",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                ],
            ),
            (
                "VNODE_OFFSET_N.facts",
                "vnode_offset_n",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                ],
            ),
        ];

        for (file_name, table_name, fields) in tables {
            let path = self.facts_dir.join(file_name);
            if path.exists() {
                let schema = Schema::new(fields);
                ctx.register_csv(
                    table_name,
                    path.to_str()
                        .ok_or_else(|| PcodeError::fact_consistency_error("Invalid path"))?,
                    CsvReadOptions::new()
                        .has_header(false)
                        .delimiter(b'\t')
                        .file_extension(".facts")
                        .schema(&schema),
                )
                .await?;
            }
        }

        let mut name_map: BTreeMap<PcodeVarnode, String> = BTreeMap::new();
        if ctx.table_exist("vnode_name")? {
            let df = ctx.sql("SELECT c0, c1 FROM vnode_name").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    name_map.insert(PcodeVarnode::from(c0.value(i)), c1.value(i).to_string());
                }
            }
        }

        let mut size_map: BTreeMap<PcodeVarnode, i64> = BTreeMap::new();
        if ctx.table_exist("vnode_size")? {
            let df = ctx.sql("SELECT c0, c1 FROM vnode_size").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
                for i in 0..batch.num_rows() {
                    size_map.insert(PcodeVarnode::from(c0.value(i)), c1.value(i));
                }
            }
        }

        let mut is_address_set: BTreeSet<PcodeVarnode> = BTreeSet::new();
        if ctx.table_exist("vnode_is_address")? {
            let df = ctx.sql("SELECT c0 FROM vnode_is_address").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    is_address_set.insert(PcodeVarnode::from(c0.value(i)));
                }
            }
        }

        let mut space_map: BTreeMap<PcodeVarnode, String> = BTreeMap::new();
        if ctx.table_exist("vnode_space")? {
            let df = ctx.sql("SELECT c0, c1 FROM vnode_space").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    space_map.insert(PcodeVarnode::from(c0.value(i)), c1.value(i).to_string());
                }
            }
        }

        let mut address_map: BTreeMap<PcodeVarnode, PcodeAddress> = BTreeMap::new();
        if ctx.table_exist("vnode_address")? {
            let df = ctx.sql("SELECT c0, c1 FROM vnode_address").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
                for i in 0..batch.num_rows() {
                    address_map.insert(PcodeVarnode::from(c0.value(i)), PcodeAddress(c1.value(i)));
                }
            }
        }

        let mut offset_map: BTreeMap<PcodeVarnode, i64> = BTreeMap::new();
        if ctx.table_exist("vnode_offset_n")? {
            let df = ctx.sql("SELECT c0, c1 FROM vnode_offset_n").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
                for i in 0..batch.num_rows() {
                    offset_map.insert(PcodeVarnode::from(c0.value(i)), c1.value(i));
                }
            }
        }

        let mut result = BTreeMap::new();
        // Combine the facts
        for (vnode_id, name) in name_map {
            let size = size_map.get(&vnode_id).cloned();
            let is_address = is_address_set.contains(&vnode_id);
            let space = space_map.get(&vnode_id).cloned();
            let address = address_map.get(&vnode_id).cloned();
            let constant_offset = offset_map.get(&vnode_id).cloned();

            result.insert(
                vnode_id,
                VnodeData {
                    name,
                    size,
                    is_address,
                    space,
                    address,
                    constant_offset,
                },
            );
        }

        Ok(result)
    }

    /// Read BB (basic block) facts
    pub fn read_bb_facts(&self) -> Result<BTreeMap<PcodeBlockBasic, BBData>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async { self.read_bb_facts_async().await })
    }

    async fn read_bb_facts_async(&self) -> Result<BTreeMap<PcodeBlockBasic, BBData>> {
        let ctx = SessionContext::new();

        let tables = [
            (
                "BB_HFUNC.facts",
                "bb_hfunc",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "BB_FIRST.facts",
                "bb_first",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "BB_LAST.facts",
                "bb_last",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "BB_START.facts",
                "bb_start",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                ],
            ),
            (
                "BB_PCODE_INDEX.facts",
                "bb_pcode_index",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Int64, false),
                    Field::new("c2", DataType::Utf8, false),
                ],
            ),
            (
                "BB_OUT.facts",
                "bb_out",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "BB_TOUT.facts",
                "bb_tout",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
            (
                "BB_FOUT.facts",
                "bb_fout",
                vec![
                    Field::new("c0", DataType::Utf8, false),
                    Field::new("c1", DataType::Utf8, false),
                ],
            ),
        ];

        for (file_name, table_name, fields) in tables {
            let path = self.facts_dir.join(file_name);
            if path.exists() {
                let schema = Schema::new(fields);
                ctx.register_csv(
                    table_name,
                    path.to_str()
                        .ok_or_else(|| PcodeError::fact_consistency_error("Invalid path"))?,
                    CsvReadOptions::new()
                        .has_header(false)
                        .delimiter(b'\t')
                        .file_extension(".facts")
                        .schema(&schema),
                )
                .await?;
            }
        }

        let mut hfunc_map: BTreeMap<PcodeBlockBasic, HighFunc> = BTreeMap::new();
        if ctx.table_exist("bb_hfunc")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_hfunc").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    hfunc_map.insert(PcodeBlockBasic::from(c0.value(i)), HighFunc::from(c1.value(i)));
                }
            }
        }

        let mut first_map: BTreeMap<PcodeBlockBasic, PcodeInstruction> = BTreeMap::new();
        if ctx.table_exist("bb_first")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_first").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    first_map.insert(PcodeBlockBasic::from(c0.value(i)), PcodeInstruction::from(c1.value(i)));
                }
            }
        }

        let mut last_map: BTreeMap<PcodeBlockBasic, PcodeInstruction> = BTreeMap::new();
        if ctx.table_exist("bb_last")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_last").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    last_map.insert(PcodeBlockBasic::from(c0.value(i)), PcodeInstruction::from(c1.value(i)));
                }
            }
        }

        let mut start_addr_map: BTreeMap<PcodeBlockBasic, PcodeAddress> = BTreeMap::new();
        if ctx.table_exist("bb_start")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_start").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
                for i in 0..batch.num_rows() {
                    start_addr_map.insert(PcodeBlockBasic::from(c0.value(i)), PcodeAddress(c1.value(i)));
                }
            }
        }

        let mut index_map: BTreeMap<PcodeBlockBasic, Vec<(u32, PcodeInstruction)>> = BTreeMap::new();
        if ctx.table_exist("bb_pcode_index")? {
            let df = ctx.sql("SELECT c0, c1, c2 FROM bb_pcode_index").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
                let c2 = batch.column(2).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    index_map.entry(PcodeBlockBasic::from(c0.value(i))).or_default().push((c1.value(i) as u32, PcodeInstruction::from(c2.value(i))));
                }
            }
        }
        for indices in index_map.values_mut() {
            indices.sort_by_key(|&(index, _)| index);
        }

        let mut out_edge_map: BTreeMap<PcodeBlockBasic, Vec<PcodeBlockBasic>> = BTreeMap::new();
        if ctx.table_exist("bb_out")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_out").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    out_edge_map.entry(PcodeBlockBasic::from(c0.value(i))).or_default().push(PcodeBlockBasic::from(c1.value(i)));
                }
            }
        }

        let mut tout_edge_map: BTreeMap<PcodeBlockBasic, Vec<PcodeBlockBasic>> = BTreeMap::new();
        if ctx.table_exist("bb_tout")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_tout").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    tout_edge_map.entry(PcodeBlockBasic::from(c0.value(i))).or_default().push(PcodeBlockBasic::from(c1.value(i)));
                }
            }
        }

        let mut fout_edge_map: BTreeMap<PcodeBlockBasic, Vec<PcodeBlockBasic>> = BTreeMap::new();
        if ctx.table_exist("bb_fout")? {
            let df = ctx.sql("SELECT c0, c1 FROM bb_fout").await?;
            let batches = df.collect().await?;
            for batch in batches {
                let c0 = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
                let c1 = batch.column(1).as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..batch.num_rows() {
                    fout_edge_map.entry(PcodeBlockBasic::from(c0.value(i))).or_default().push(PcodeBlockBasic::from(c1.value(i)));
                }
            }
        }

        let mut result = BTreeMap::new();
        // Combine the facts
        for (bb_id, hfunc_id) in hfunc_map {
            let first_inst = first_map.get(&bb_id).cloned();
            let last_inst = last_map.get(&bb_id).cloned();
            let start_address = start_addr_map.get(&bb_id).cloned();
            let instruction_indices = index_map.get(&bb_id).cloned().unwrap_or_default();
            let out_edges = out_edge_map.get(&bb_id).cloned().unwrap_or_default();
            let tout_edges = tout_edge_map.get(&bb_id).cloned().unwrap_or_default();
            let fout_edges = fout_edge_map.get(&bb_id).cloned().unwrap_or_default();

            result.insert(
                bb_id,
                BBData {
                    hfunc: hfunc_id,
                    start_address,
                    first_inst,
                    last_inst,
                    instruction_indices,
                    out_edges,
                    tout_edges,
                    fout_edges,
                },
            );
        }

        Ok(result)
    }

    /// Read SYMBOL_HVAR facts
    pub fn read_symbol_hvar_facts(&self) -> Result<BTreeMap<HighSymbol, HighVariable>> {
        let facts = self.read_csv_facts::<(HighSymbol, HighVariable)>("SYMBOL_HVAR.facts")?;
        Ok(facts.into_iter().collect())
    }

    /// Read HVAR_NAME facts
    pub fn read_hvar_name_facts(&self) -> Result<BTreeMap<HighVariable, String>> {
        let facts = self.read_csv_facts::<(HighVariable, String)>("HVAR_NAME.facts")?;
        Ok(facts.into_iter().collect())
    }

    /// Read HVAR_REPRESENTATIVE facts
    pub fn read_hvar_representative_facts(&self) -> Result<BTreeMap<HighVariable, PcodeVarnode>> {
        let facts =
            self.read_csv_facts::<(HighVariable, PcodeVarnode)>("HVAR_REPRESENTATIVE.facts")?;
        Ok(facts.into_iter().collect())
    }

    /// Helper function to read CSV facts
    fn read_csv_facts<T: serde::de::DeserializeOwned>(&self, filename: &str) -> Result<Vec<T>> {
        log::trace!("read_csv_facts: {filename}");
        self.read_csv_facts_optional(filename)?
            .ok_or_else(|| PcodeError::missing_fact_file(self.facts_dir.join(filename)))
    }

    /// Helper function to read CSV facts optionally (returns None if file doesn't exist)
    fn read_csv_facts_optional<T: serde::de::DeserializeOwned>(
        &self,
        filename: &str,
    ) -> Result<Option<Vec<T>>> {
        let path = self.facts_dir.join(filename);
        if !path.exists() {
            return Ok(None);
        }

        let file = std::fs::File::open(&path).map_err(PcodeError::from)?;

        let reader = std::io::BufReader::new(file);
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .delimiter(b'\t') // Use tab delimiter
            .from_reader(reader);

        let mut result = Vec::new();
        for record_result in rdr.deserialize() {
            let record: T = record_result.map_err(|e| PcodeError::csv_parse_error(filename, e))?;
            result.push(record);
        }

        Ok(Some(result))
    }
}

#[derive(Debug, Default)]
pub struct PcodeFacts {
    pub hfunc_facts: BTreeMap<HighFunc, HFuncData>,
    pub pcode_facts: BTreeMap<PcodeInstruction, PcodeData>,
    pub vnode_facts: BTreeMap<PcodeVarnode, VnodeData>,
    pub bb_facts: BTreeMap<PcodeBlockBasic, BBData>,
    pub proto_facts: BTreeMap<HighProto, ProtoData>,
    pub symbol_hvar_facts: BTreeMap<HighSymbol, HighVariable>,
    pub hvar_name_facts: BTreeMap<HighVariable, String>,
    pub hvar_representative_facts: BTreeMap<HighVariable, PcodeVarnode>,
    pub register_facts: Vec<RegisterData>,
}

impl PcodeFacts {
    /// Create an empty PcodeFacts collection
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the representative PcodeVarnode for a given HighSymbol
    /// by chaining symbol_hvar_facts and hvar_representative_facts mappings.
    ///
    /// Returns the first PcodeVarnode found by:
    /// 1. Looking up the HighSymbol in symbol_hvar_facts to get a HighVariable
    /// 2. Looking up the HighVariable in hvar_representative_facts to get a PcodeVarnode
    ///
    /// Returns None if either mapping is not found.
    pub fn get_symbol_representative(&self, symbol: &HighSymbol) -> Option<&PcodeVarnode> {
        // Step 1: Find HighVariable for the given HighSymbol
        let hvar = self.symbol_hvar_facts.get(symbol)?;

        // Step 2: Find PcodeVarnode for the HighVariable
        self.hvar_representative_facts.get(hvar)
    }
}
