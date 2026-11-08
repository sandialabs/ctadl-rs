//! Pcode reader library
//!
//! This crate provides functionality for reading and parsing Ghidra pcode facts.
//! It's designed to be used by the CTADL pcode frontend to convert pcode facts
//! into CTADL IR.

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;
use std::fmt::Display;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use internment::ArcIntern;
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet};

pub use error::PcodeError;

pub mod constant_propagation;
pub mod error;
mod table;

/// Runs each expression on the rayon pool, in parallel, and returns their values as nested
/// pairs: `join!(a, b, c)` is `(a, (b, c))`.
macro_rules! join {
    ($a:expr $(,)?) => { $a };
    ($a:expr, $($rest:expr),+ $(,)?) => { rayon::join(|| $a, || join!($($rest),+)) };
}

/// Macro to generate newtype wrappers around `ArcIntern<str>`.
/// Each newtype gets `From<impl AsRef<str>>`, `Deref` and `Display` implementations.
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

/// Result type for pcode-reader operations.
pub type Result<T> = std::result::Result<T, PcodeError>;

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

    /// Resolve a logical fact filename (e.g. `PCODE_INPUT.facts`) to the file that
    /// actually exists on disk.
    ///
    /// The plain `<filename>` is preferred when present (returning `false` for
    /// "compressed"); otherwise the gzipped `<filename>.gz` is used. A plain table is
    /// either one Ghidra just wrote, which a `.gz` left by an earlier export may be older
    /// than and which is still being compressed while it is read, or one someone
    /// unpacked by hand; either way it is current, and reading it skips inflating it.
    /// Returns `None` if neither variant exists.
    fn resolve_fact_path(&self, filename: &str) -> Option<(PathBuf, bool)> {
        let plain = self.facts_dir.join(filename);
        if plain.exists() {
            return Some((plain, false));
        }
        let gz = self.facts_dir.join(format!("{filename}.gz"));
        if gz.exists() {
            return Some((gz, true));
        }
        None
    }

    /// The rows of the table `filename`, each mapped through `f`, in file order; `None` when
    /// the table is absent.
    fn rows_optional<const N: usize, T, F>(&self, filename: &str, f: F) -> Result<Option<Vec<T>>>
    where
        T: Send,
        F: Fn([&str; N]) -> Result<T> + Sync,
    {
        log::trace!("pcode: reading {filename}");
        let Some((path, compressed)) = self.resolve_fact_path(filename) else {
            return Ok(None);
        };
        let bytes = table::load(&path, compressed)?;
        table::parse(filename, &bytes, f).map(Some)
    }

    /// Like [`Self::rows_optional`], for a table that must be present.
    fn rows<const N: usize, T, F>(&self, filename: &str, f: F) -> Result<Vec<T>>
    where
        T: Send,
        F: Fn([&str; N]) -> Result<T> + Sync,
    {
        self.rows_optional(filename, f)?
            .ok_or_else(|| PcodeError::missing_fact_file(self.facts_dir.join(filename)))
    }

    /// Like [`Self::rows_optional`], with an absent table read as empty.
    fn rows_or_empty<const N: usize, T, F>(&self, filename: &str, f: F) -> Result<Vec<T>>
    where
        T: Send,
        F: Fn([&str; N]) -> Result<T> + Sync,
    {
        Ok(self.rows_optional(filename, f)?.unwrap_or_default())
    }

    /// Read all essential pcode facts. The tables are read in parallel, and so is each large
    /// one.
    pub fn read_all_facts(&self) -> Result<PcodeFacts> {
        let (pcode_facts, (vnode_facts, (bb_facts, (vnode_hvar_facts, rest)))) = join!(
            self.read_pcode_facts(),
            self.read_vnode_facts(),
            self.read_bb_facts(),
            self.read_vnode_hvar_facts(),
            join!(
                self.read_hfunc_facts(),
                self.read_proto_facts(),
                self.read_symbol_hvar_facts(),
                self.read_hvar_name_facts(),
                self.read_hvar_representative_facts(),
                self.read_hvar_class_facts(),
                self.read_hvar_type_facts(),
                self.read_type_kinds(),
                self.read_register_facts()
            )
        );
        let (
            hfunc_facts,
            (
                proto_facts,
                (
                    symbol_hvar_facts,
                    (
                        hvar_name_facts,
                        (
                            hvar_representative_facts,
                            (hvar_class_facts, (hvar_type_facts, (type_kinds, register_facts))),
                        ),
                    ),
                ),
            ),
        ) = rest;
        Ok(PcodeFacts {
            hfunc_facts: hfunc_facts?,
            pcode_facts: pcode_facts?,
            vnode_facts: vnode_facts?,
            bb_facts: bb_facts?,
            proto_facts: proto_facts?,
            symbol_hvar_facts: symbol_hvar_facts?,
            hvar_name_facts: hvar_name_facts?,
            hvar_representative_facts: hvar_representative_facts?,
            vnode_hvar_facts: vnode_hvar_facts?,
            hvar_class_facts: hvar_class_facts?,
            hvar_type_facts: hvar_type_facts?,
            type_kinds: type_kinds?,
            register_facts: register_facts?,
        })
    }

    /// Read the program's image base: the address Ghidra loaded the artifact at.
    /// Returns `None` if the fact is absent (older fact sets predate it).
    pub fn read_image_base(&self) -> Result<Option<i64>> {
        const FILE: &str = "PROGRAM_IMAGE_BASE.facts";
        let rows = self.rows_optional(FILE, |[base]| table::int(FILE, base))?;
        Ok(rows.and_then(|rows| rows.into_iter().next()))
    }

    /// Read Register facts
    pub fn read_register_facts(&self) -> Result<Vec<RegisterData>> {
        const FILE: &str = "REGISTER_OFF_NAME.facts";
        let (off_name_facts, is_sp_facts) = join!(
            self.rows_or_empty(FILE, |[offset, size, name]| {
                Ok((
                    table::int(FILE, offset)?,
                    table::int(FILE, size)?,
                    name.to_string(),
                ))
            }),
            self.rows_or_empty("REGISTER_IS_SP.facts", |[name]| Ok(name.to_string()))
        );
        let sp_set: BTreeSet<String> = is_sp_facts?.into_iter().collect();

        let mut result = Vec::new();
        for (offset, size, name) in off_name_facts? {
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
        const EP: &str = "HFUNC_EP.facts";
        const LOCAL_EP: &str = "HFUNC_LOCAL_EP.facts";
        let (name_facts, (proto_facts, (isext_facts, (ep_facts, local_ep_facts)))) = join!(
            self.rows("HFUNC_NAME.facts", |[f, name]| Ok((
                HighFunc::from(f),
                name.to_string()
            ))),
            self.rows("HFUNC_PROTO.facts", |[f, proto]| Ok((
                HighFunc::from(f),
                HighProto::from(proto)
            ))),
            self.rows("HFUNC_ISEXT.facts", |[f]| Ok(HighFunc::from(f))),
            self.rows(EP, |[f, ep]| Ok((HighFunc::from(f), table::int(EP, ep)?))),
            self.rows(LOCAL_EP, |[ep, local]| Ok((
                table::int(LOCAL_EP, ep)?,
                table::int(LOCAL_EP, local)?
            )))
        );

        let name_map: BTreeMap<HighFunc, String> = name_facts?.into_iter().collect();
        let proto_map: BTreeMap<HighFunc, HighProto> = proto_facts?.into_iter().collect();
        let isext_set: BTreeSet<HighFunc> = isext_facts?.into_iter().collect();

        let mut ep_map: BTreeMap<HighFunc, PcodeAddress> = BTreeMap::new();
        let mut ep_to_func_map: BTreeMap<PcodeAddress, HighFunc> = BTreeMap::new();
        for (func_id, ep_address_val) in ep_facts? {
            let ep_address = PcodeAddress(ep_address_val);
            ep_map.insert(func_id.clone(), ep_address.clone());
            ep_to_func_map.insert(ep_address, func_id);
        }

        let mut local_ep_map: BTreeMap<HighFunc, Vec<PcodeAddress>> = BTreeMap::new();
        for (ep_val, local_ep_val) in local_ep_facts? {
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

        let mut result = BTreeMap::new();
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
        const COUNT: &str = "PROTO_PARAMETER_COUNT.facts";
        const PARAM: &str = "PROTO_PARAMETER.facts";
        let (param_count_facts, (param_facts, (rettype_facts, (vararg_facts, void_facts)))) = join!(
            self.rows(COUNT, |[p, n]| Ok((HighProto::from(p), count(COUNT, n)?))),
            self.rows(PARAM, |[p, i, s]| Ok((
                HighProto::from(p),
                count(PARAM, i)?,
                HighSymbol::from(s)
            ))),
            self.rows("PROTO_RETTYPE.facts", |[p, ty]| Ok((
                HighProto::from(p),
                ty.to_string()
            ))),
            self.rows("PROTO_IS_VARARG.facts", |[p]| Ok(HighProto::from(p))),
            self.rows("PROTO_IS_VOID.facts", |[p]| Ok(HighProto::from(p)))
        );

        let param_count_map: BTreeMap<HighProto, usize> = param_count_facts?.into_iter().collect();
        let mut param_map: BTreeMap<HighProto, Vec<(usize, HighSymbol)>> = BTreeMap::new();
        for (proto_id, index, symbol) in param_facts? {
            param_map.entry(proto_id).or_default().push((index, symbol));
        }
        let rettype_map: BTreeMap<HighProto, String> = rettype_facts?.into_iter().collect();
        let vararg_set: BTreeSet<HighProto> = vararg_facts?.into_iter().collect();
        let void_set: BTreeSet<HighProto> = void_facts?.into_iter().collect();

        let mut result = BTreeMap::new();
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
        let mut sorted_params = param_map.get(proto_id).cloned().unwrap_or_default();
        sorted_params.sort_by_key(|&(idx, _)| idx);

        let mut parameters = Vec::with_capacity(param_count);
        for (_, symbol) in sorted_params {
            parameters.push(ProtoParameter { symbol });
        }
        parameters
    }

    /// Read PCODE facts
    pub fn read_pcode_facts(&self) -> Result<BTreeMap<PcodeInstruction, PcodeData>> {
        const INPUT: &str = "PCODE_INPUT.facts";
        const INDEX: &str = "PCODE_INDEX.facts";
        const TARGET: &str = "PCODE_TARGET.facts";
        let (mnemonics, (opcodes, (inputs, (outputs, (indices, (targets, (parents, bbs))))))) = join!(
            self.rows_or_empty("PCODE_MNEMONIC.facts", |[i, m]| Ok((
                PcodeInstruction::from(i),
                PcodeMnemonic::from(m)
            ))),
            self.rows_or_empty("PCODE_OPCODE.facts", |[i, op]| Ok((
                PcodeInstruction::from(i),
                op.to_string()
            ))),
            self.rows_or_empty(INPUT, |[i, n, vn]| Ok((
                PcodeInstruction::from(i),
                table::int(INPUT, n)?,
                PcodeVarnode::from(vn)
            ))),
            self.rows_or_empty("PCODE_OUTPUT.facts", |[i, vn]| Ok((
                PcodeInstruction::from(i),
                PcodeVarnode::from(vn)
            ))),
            self.rows_or_empty(INDEX, |[i, n]| Ok((
                PcodeInstruction::from(i),
                table::int(INDEX, n)?
            ))),
            self.rows_or_empty(TARGET, |[i, a]| Ok((
                PcodeInstruction::from(i),
                PcodeAddress(table::int(TARGET, a)?)
            ))),
            self.rows_or_empty("PCODE_PARENT.facts", |[i, bb]| Ok((
                PcodeInstruction::from(i),
                PcodeBlockBasic::from(bb)
            ))),
            self.rows_or_empty("BB_HFUNC.facts", |[bb, _]| Ok(PcodeBlockBasic::from(bb)))
        );

        let (mnemonics, opcodes, inputs, outputs) = (mnemonics?, opcodes?, inputs?, outputs?);
        let (indices, targets, parents, bbs) = (indices?, targets?, parents?, bbs?);
        let (mnemonics, (opcodes, (inputs, (outputs, (indices, (targets, (parents, bb_ids))))))) = join!(
            sorted(mnemonics),
            last_wins(opcodes),
            {
                let mut inputs_by_inst: HashMap<
                    PcodeInstruction,
                    SmallVec<[(i64, PcodeVarnode); 2]>,
                > = HashMap::new();
                for (inst, index, vn) in inputs {
                    inputs_by_inst.entry(inst).or_default().push((index, vn));
                }
                inputs_by_inst
                    .into_iter()
                    .map(|(inst, mut inputs)| {
                        inputs.sort_by_key(|&(index, _)| index);
                        (inst, inputs.into_iter().map(|(_, vn)| vn).collect())
                    })
                    .collect::<HashMap<PcodeInstruction, SmallVec<[PcodeVarnode; 2]>>>()
            },
            {
                let mut outputs_by_inst: HashMap<PcodeInstruction, SmallVec<[PcodeVarnode; 1]>> =
                    HashMap::new();
                for (inst, vn) in outputs {
                    outputs_by_inst.entry(inst).or_default().push(vn);
                }
                outputs_by_inst
            },
            last_wins(indices),
            last_wins(targets),
            last_wins(parents),
            bbs.into_iter().collect::<HashSet<PcodeBlockBasic>>()
        );

        let rows: Vec<(PcodeInstruction, PcodeData)> = mnemonics
            .into_par_iter()
            .map(|(inst_id, mnemonic)| {
                let data = PcodeData {
                    mnemonic,
                    opcode: opcodes.get(&inst_id).cloned(),
                    inputs: inputs.get(&inst_id).cloned().unwrap_or_default(),
                    outputs: outputs.get(&inst_id).cloned().unwrap_or_default(),
                    bb_id: parents
                        .get(&inst_id)
                        .filter(|id| bb_ids.contains(*id))
                        .cloned(),
                    index: indices.get(&inst_id).copied().unwrap_or(0),
                    target: targets.get(&inst_id).cloned(),
                };
                (inst_id, data)
            })
            .collect();
        Ok(rows.into_iter().collect())
    }

    /// Read VNODE facts
    pub fn read_vnode_facts(&self) -> Result<BTreeMap<PcodeVarnode, VnodeData>> {
        const SIZE: &str = "VNODE_SIZE.facts";
        const ADDRESS: &str = "VNODE_ADDRESS.facts";
        const OFFSET: &str = "VNODE_OFFSET_N.facts";
        let (names, (sizes, (is_address, (spaces, (addresses, offsets))))) = join!(
            self.rows_or_empty("VNODE_NAME.facts", |[vn, name]| Ok((
                PcodeVarnode::from(vn),
                name.to_string()
            ))),
            self.rows_or_empty(SIZE, |[vn, n]| Ok((
                PcodeVarnode::from(vn),
                table::int(SIZE, n)?
            ))),
            self.rows_or_empty("VNODE_IS_ADDRESS.facts", |[vn]| Ok(PcodeVarnode::from(vn))),
            self.rows_or_empty("VNODE_SPACE.facts", |[vn, space]| Ok((
                PcodeVarnode::from(vn),
                space.to_string()
            ))),
            self.rows_or_empty(ADDRESS, |[vn, a]| Ok((
                PcodeVarnode::from(vn),
                PcodeAddress(table::int(ADDRESS, a)?)
            ))),
            self.rows_or_empty(OFFSET, |[vn, n]| Ok((
                PcodeVarnode::from(vn),
                table::int(OFFSET, n)?
            )))
        );

        let (names, sizes, is_address) = (names?, sizes?, is_address?);
        let (spaces, addresses, offsets) = (spaces?, addresses?, offsets?);
        let (names, (sizes, (is_address, (spaces, (addresses, offsets))))) = join!(
            sorted(names),
            last_wins(sizes),
            is_address.into_iter().collect::<HashSet<PcodeVarnode>>(),
            last_wins(spaces),
            last_wins(addresses),
            last_wins(offsets)
        );

        let rows: Vec<(PcodeVarnode, VnodeData)> = names
            .into_par_iter()
            .map(|(vnode_id, name)| {
                let data = VnodeData {
                    name,
                    size: sizes.get(&vnode_id).copied(),
                    is_address: is_address.contains(&vnode_id),
                    space: spaces.get(&vnode_id).cloned(),
                    address: addresses.get(&vnode_id).cloned(),
                    constant_offset: offsets.get(&vnode_id).copied(),
                };
                (vnode_id, data)
            })
            .collect();
        Ok(rows.into_iter().collect())
    }

    /// Read BB (basic block) facts
    pub fn read_bb_facts(&self) -> Result<BTreeMap<PcodeBlockBasic, BBData>> {
        const START: &str = "BB_START.facts";
        const INDEX: &str = "BB_PCODE_INDEX.facts";
        let (hfuncs, (firsts, (lasts, (starts, (indices, (outs, (touts, fouts))))))) = join!(
            self.rows_or_empty("BB_HFUNC.facts", |[bb, f]| Ok((
                PcodeBlockBasic::from(bb),
                HighFunc::from(f)
            ))),
            self.rows_or_empty("BB_FIRST.facts", |[bb, i]| Ok((
                PcodeBlockBasic::from(bb),
                PcodeInstruction::from(i)
            ))),
            self.rows_or_empty("BB_LAST.facts", |[bb, i]| Ok((
                PcodeBlockBasic::from(bb),
                PcodeInstruction::from(i)
            ))),
            self.rows_or_empty(START, |[bb, a]| Ok((
                PcodeBlockBasic::from(bb),
                PcodeAddress(table::int(START, a)?)
            ))),
            self.rows_or_empty(INDEX, |[bb, n, i]| Ok((
                PcodeBlockBasic::from(bb),
                table::int(INDEX, n)? as u32,
                PcodeInstruction::from(i)
            ))),
            self.rows_or_empty("BB_OUT.facts", |[bb, to]| Ok((
                PcodeBlockBasic::from(bb),
                PcodeBlockBasic::from(to)
            ))),
            self.rows_or_empty("BB_TOUT.facts", |[bb, to]| Ok((
                PcodeBlockBasic::from(bb),
                PcodeBlockBasic::from(to)
            ))),
            self.rows_or_empty("BB_FOUT.facts", |[bb, to]| Ok((
                PcodeBlockBasic::from(bb),
                PcodeBlockBasic::from(to)
            )))
        );

        let (hfuncs, firsts, lasts, starts) = (hfuncs?, firsts?, lasts?, starts?);
        let (indices, outs, touts, fouts) = (indices?, outs?, touts?, fouts?);
        let (hfuncs, (firsts, (lasts, (starts, (indices, (outs, (touts, fouts))))))) = join!(
            sorted(hfuncs),
            last_wins(firsts),
            last_wins(lasts),
            last_wins(starts),
            {
                let mut index_map: HashMap<PcodeBlockBasic, Vec<(u32, PcodeInstruction)>> =
                    HashMap::new();
                for (bb, index, inst) in indices {
                    index_map.entry(bb).or_default().push((index, inst));
                }
                for indices in index_map.values_mut() {
                    indices.sort_by_key(|&(index, _)| index);
                }
                index_map
            },
            grouped(outs),
            grouped(touts),
            grouped(fouts)
        );

        let rows: Vec<(PcodeBlockBasic, BBData)> = hfuncs
            .into_par_iter()
            .map(|(bb_id, hfunc)| {
                let data = BBData {
                    hfunc,
                    start_address: starts.get(&bb_id).cloned(),
                    first_inst: firsts.get(&bb_id).cloned(),
                    last_inst: lasts.get(&bb_id).cloned(),
                    instruction_indices: indices.get(&bb_id).cloned().unwrap_or_default(),
                    out_edges: outs.get(&bb_id).cloned().unwrap_or_default(),
                    tout_edges: touts.get(&bb_id).cloned().unwrap_or_default(),
                    fout_edges: fouts.get(&bb_id).cloned().unwrap_or_default(),
                };
                (bb_id, data)
            })
            .collect();
        Ok(rows.into_iter().collect())
    }

    /// Read SYMBOL_HVAR facts
    pub fn read_symbol_hvar_facts(&self) -> Result<BTreeMap<HighSymbol, Vec<HighVariable>>> {
        let facts = self.rows("SYMBOL_HVAR.facts", |[s, h]| {
            Ok((HighSymbol::from(s), HighVariable::from(h)))
        })?;
        let mut m: BTreeMap<HighSymbol, Vec<HighVariable>> = BTreeMap::new();
        for (s, h) in facts {
            m.entry(s).or_default().push(h);
        }
        Ok(m)
    }

    /// Read HVAR_NAME facts
    pub fn read_hvar_name_facts(&self) -> Result<BTreeMap<HighVariable, String>> {
        let facts = self.rows("HVAR_NAME.facts", |[h, name]| {
            Ok((HighVariable::from(h), name.to_string()))
        })?;
        Ok(sorted(facts).into_iter().collect())
    }

    /// Read HVAR_REPRESENTATIVE facts
    pub fn read_hvar_representative_facts(&self) -> Result<BTreeMap<HighVariable, PcodeVarnode>> {
        let facts = self.rows("HVAR_REPRESENTATIVE.facts", |[h, vn]| {
            Ok((HighVariable::from(h), PcodeVarnode::from(vn)))
        })?;
        Ok(sorted(facts).into_iter().collect())
    }

    /// Read VNODE_HVAR facts, mapping each varnode to the high variable it belongs to.
    ///
    /// Optional: returns an empty map when the fact file is absent, as it is in older
    /// fact sets.
    pub fn read_vnode_hvar_facts(&self) -> Result<BTreeMap<PcodeVarnode, HighVariable>> {
        let facts = self.rows_or_empty("VNODE_HVAR.facts", |[vn, h]| {
            Ok((PcodeVarnode::from(vn), HighVariable::from(h)))
        })?;
        Ok(sorted(facts).into_iter().collect())
    }

    /// Read HVAR_CLASS facts, giving Ghidra's storage classification for each high
    /// variable (`global`, `constant`, `other`, ...).
    ///
    /// Optional, for the same reason as [`Self::read_vnode_hvar_facts`].
    pub fn read_hvar_class_facts(&self) -> Result<BTreeMap<HighVariable, String>> {
        let facts = self.rows_or_empty("HVAR_CLASS.facts", |[h, class]| {
            Ok((HighVariable::from(h), class.to_string()))
        })?;
        Ok(sorted(facts).into_iter().collect())
    }

    /// Read HVAR_TYPE facts, giving the id of the data type Ghidra assigned each high
    /// variable.
    ///
    /// Optional, for the same reason as [`Self::read_vnode_hvar_facts`].
    pub fn read_hvar_type_facts(&self) -> Result<BTreeMap<HighVariable, String>> {
        let facts = self.rows_or_empty("HVAR_TYPE.facts", |[h, ty]| {
            Ok((HighVariable::from(h), ty.to_string()))
        })?;
        Ok(sorted(facts).into_iter().collect())
    }

    /// Read the kind of each exported data type id from the `TYPE_POINTER`,
    /// `TYPE_INTEGER`, `TYPE_BOOLEAN`, `TYPE_FLOAT` and `TYPE_ENUM` facts. The exporter
    /// resolves typedefs before classifying, so a typedef id gets its base type's kind.
    /// Types in none of these files (Ghidra's `undefinedN`, structs, arrays) are absent.
    pub fn read_type_kinds(&self) -> Result<BTreeMap<String, TypeKind>> {
        let mut kinds = BTreeMap::new();
        for (file, kind) in [
            ("TYPE_INTEGER.facts", TypeKind::Scalar),
            ("TYPE_BOOLEAN.facts", TypeKind::Scalar),
            ("TYPE_FLOAT.facts", TypeKind::Scalar),
            ("TYPE_ENUM.facts", TypeKind::Scalar),
            // Last, so a type that is somehow both is a pointer.
            ("TYPE_POINTER.facts", TypeKind::Pointer),
        ] {
            for id in self.rows_or_empty(file, |[id]| Ok(id.to_string()))? {
                kinds.insert(id, kind);
            }
        }
        Ok(kinds)
    }
}

/// `rows` sorted by key, stably, so that collecting them into a map keeps the last row of
/// each key as inserting them in order would; the collect then has nothing left to sort.
fn sorted<K: Ord + Send, V: Send>(mut rows: Vec<(K, V)>) -> Vec<(K, V)> {
    rows.par_sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// `rows` as a map in which a later row of a key replaces an earlier one.
fn last_wins<K: Eq + std::hash::Hash, V>(rows: Vec<(K, V)>) -> HashMap<K, V> {
    rows.into_iter().collect()
}

/// `rows` grouped by key, each group in file order.
fn grouped<K: Eq + std::hash::Hash, V>(rows: Vec<(K, V)>) -> HashMap<K, Vec<V>> {
    let mut groups: HashMap<K, Vec<V>> = HashMap::new();
    for (k, v) in rows {
        groups.entry(k).or_default().push(v);
    }
    groups
}

/// A count or position field.
fn count(filename: &str, field: &str) -> Result<usize> {
    usize::try_from(table::int(filename, field)?)
        .map_err(|e| PcodeError::invalid_fact_format(filename, format!("bad count {field:?}: {e}")))
}

/// What Ghidra's data type says a value is, as far as pointer flow cares.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TypeKind {
    /// A pointer type.
    Pointer,
    /// An integer, boolean, float or enum type: a value that is not an address.
    Scalar,
}

#[derive(Debug, Default)]
pub struct PcodeFacts {
    pub hfunc_facts: BTreeMap<HighFunc, HFuncData>,
    pub pcode_facts: BTreeMap<PcodeInstruction, PcodeData>,
    pub vnode_facts: BTreeMap<PcodeVarnode, VnodeData>,
    pub bb_facts: BTreeMap<PcodeBlockBasic, BBData>,
    pub proto_facts: BTreeMap<HighProto, ProtoData>,
    pub symbol_hvar_facts: BTreeMap<HighSymbol, Vec<HighVariable>>,
    pub hvar_name_facts: BTreeMap<HighVariable, String>,
    pub hvar_representative_facts: BTreeMap<HighVariable, PcodeVarnode>,
    pub vnode_hvar_facts: BTreeMap<PcodeVarnode, HighVariable>,
    pub hvar_class_facts: BTreeMap<HighVariable, String>,
    pub hvar_type_facts: BTreeMap<HighVariable, String>,
    pub type_kinds: BTreeMap<String, TypeKind>,
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
    /// Returns the representative of the first of the symbol's HighVariables (in
    /// symbol_hvar_facts order) that has one, or None if none does.
    pub fn get_symbol_representative(&self, symbol: &HighSymbol) -> Option<&PcodeVarnode> {
        // Ghidra maps one symbol to several high variables when it splits an aggregate
        // into the pieces the body actually touches (the whole variable, plus one per
        // accessed field). Only some of them carry a representative varnode, so try each
        // rather than picking one blindly.
        let hvars = self.symbol_hvar_facts.get(symbol)?;
        hvars
            .iter()
            .find_map(|hvar| self.hvar_representative_facts.get(hvar))
    }

    /// The kind of `vnode`'s data type, through its high variable. `None` when the
    /// varnode has no high variable, or its type is neither a pointer nor a scalar
    /// (for example Ghidra's `undefined4`).
    pub fn vnode_type_kind(&self, vnode: &PcodeVarnode) -> Option<TypeKind> {
        let hvar = self.vnode_hvar_facts.get(vnode)?;
        let ty = self.hvar_type_facts.get(hvar)?;
        self.type_kinds.get(ty).copied()
    }

    /// Whether `vnode` is storage for a global variable, i.e. whether it is a `ram`
    /// varnode of a high variable the decompiler typed as a `HighGlobal`.
    ///
    /// Neither test alone is enough. Call-target varnodes also live in the `ram` space,
    /// and are distinguished by having no high variable at all. And Ghidra puts some
    /// `unique` and `register` temps holding the global's value into its high variable;
    /// their address is a unique-space offset or a register number, which unrelated
    /// globals share, so they are not the global's storage. Each function gets its own
    /// high variable for a given global, so callers must key the global's identity on
    /// its address rather than on the hvar.
    pub fn is_global_vnode(&self, vnode: &PcodeVarnode) -> bool {
        self.vnode_facts
            .get(vnode)
            .is_some_and(|data| data.space.as_deref() == Some("ram"))
            && self
                .vnode_hvar_facts
                .get(vnode)
                .and_then(|hvar| self.hvar_class_facts.get(hvar))
                .is_some_and(|class| class == "global")
    }
}
