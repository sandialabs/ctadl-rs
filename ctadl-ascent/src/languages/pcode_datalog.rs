//! Translate pcode facts into IR
//!
//!
use std::{
    collections::HashSet,
    path,
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Context;
use itertools::Itertools;
use rayon::prelude::*;

use crate::{
    Config, DetectLanguage, facts,
    facts::{
        FlowType, Function, Insn, NamespaceChild, NamespaceParent, Region, Str, Uri, read_csv,
    },
    languages::{LanguageFactsReader, TargetMetadata, ir},
};

use ascent::aggregators::min;

pub struct PcodeLanguageFacts;

impl LanguageFactsReader for PcodeLanguageFacts {

    /// Reads the original facts to get some function metadata
    fn func_info(&mut self, factsdir: &path::Path) -> anyhow::Result<TargetMetadata> {
        let method: Vec<(Str, Str)> = 
	    read_csv(&factsdir.join("HFUNC_NAME.facts"))
	        .with_context(|| {
	            format!(
	                "Failed attempting to read original program facts: {}",
	                factsdir.display()
	            )
	        })?
	       .collect();
        let method_sig: Vec<(Str, Str)> = 
	    read_csv(&factsdir.join("HFUNC_PROTO.facts"))
	        .with_context(|| {
	            format!(
	                "Failed attempting to read original program facts: {}",
	                factsdir.display()
	            )
	        })?
	       .collect();
        let method_implemented: Vec<(Str, Str, Str, Str)> = Vec::new();
        /*
            read_csv(&factsdir.join("MethodImplemented.facts"))
                .with_context(|| {
                    format!(
                        "Failed attempting to read original program facts: {}",
                        factsdir.display()
                    )
                })?
                .collect();
        */
        let method_invocation: Vec<(Str, Str, Str, Str, Str, Str, Str)> = Vec::new();
        /*
            read_csv(&factsdir.join("MethodInvocation.facts"))
                .with_context(|| {
                    format!(
                        "Failed attempting to read original program facts: {}",
                        factsdir.display()
                    )
                })?
                .collect();
        */
        let result = ascent::ascent_run! {
            relation method(Str, Str) = method;
            relation method_sig(Str, Str) = method_sig;
            relation method_implemented(Str, Str, Str, Str) = method_implemented;
            relation method_invocation(Str, Str, Str, Str, Str, Str, Str) = method_invocation;
            relation funcs(Function);
            relation func_sig(Function, Str);
            relation func_name(Function, Str);
            relation namespace_parent(NamespaceChild, NamespaceParent);

            funcs(f.clone().into()) <-- method(f, _);
            func_sig(f.clone().into(), s) <-- method_sig(f, s);
            func_name(f.clone().into(), n) <-- method(f, n);

            namespace_parent(NamespaceChild(c.clone()), NamespaceParent(p.clone())) <--
                method_invocation(_, c, _, p, _, _, _);
            namespace_parent(NamespaceChild(c.clone()), NamespaceParent(p.clone())) <--
                method_implemented(p, _, _, c);
            //namespace_parent(NamespaceChild(c.clone()), NamespaceParent(p.clone())) <--
            //    method(c, _, p, _, _, _);
        };
        Ok(TargetMetadata {
            funcs: result.funcs,
            func_sig: result.func_sig,
            func_name: result.func_name,
            namespace_parent: result.namespace_parent,
        })
    }

    /// Some models are better expressed as datalog and we implement those here
    fn make_models(
        &self,
        dir: &path::Path,
    ) -> anyhow::Result<Vec<(facts::InsnSite, facts::Function)>> {
        use crate::facts::{Function, Insn, InsnSite};
        use std::path;

        let mut method_invocation: Vec<(Str, Str, Str, Str, Str, Str, Str)> = Vec::new();
        let mut method: Vec<(Str, Str)> = Vec::new();
        let mut stmt_in_method: Vec<(Str, Str)> = Vec::new();
        let path = path::Path::new(dir.as_os_str());
        method.extend(
            read_csv(&path.join("HFUNC_NAME.facts"))
                .with_context(|| format!("{:?}", path.join("HFUNC_NAME.facts").as_os_str()))?,
        );
        /*
        method_invocation.extend(
            read_csv(&path.join("MethodInvocation.facts")).with_context(|| {
                format!("{:?}", path.join("MethodInvocation.facts").as_os_str())
            })?,
        );
        stmt_in_method.extend(
            read_csv(&path.join("StmtInMethod.facts"))
                .with_context(|| format!("{:?}", path.join("StmtInMethod.facts").as_os_str()))?
                .map(|(s, _, f): (Str, i64, Str)| (s, f)),
        );
        */
        let result = ascent::ascent_run! {
            relation method_invocation(Str, Str, Str, Str, Str, Str, Str) = method_invocation;
            relation method(Str, Str) = method;
            relation stmt_in_method(Str, Str) = stmt_in_method;

            relation synth_call(InsnSite, Function);

            synth_call(site, do_in_bg_f) <--
                // Find calls to execute that return tasks
                method_invocation(insn, f, _, decl_ty, _, _, _),
                if f.contains(".execute:"),
                if f.ends_with("Landroid/os/AsyncTask;"),
                // Find the doInBackground method of the same class
                method(do_in_bg, name),
                if name == "doInBackground",
                // Synth a call to doInBackground
                stmt_in_method(insn, inmeth),
                let site = InsnSite(Function(inmeth.clone()), Insn(insn.clone())),
                let do_in_bg_f = Function(do_in_bg.clone());
        }
        .synth_call;
        Ok(result)
    }

    /// Reads original facts to get decompiled source and bytecode offset locations for program
    /// instructions
    fn insn_location(
        &mut self,
        config: &Config,
        tainted_insn: &[Insn],
    ) -> anyhow::Result<Vec<(Insn, Uri, Region, Option<Uri>, Option<Region>)>> {
        use facts::*;
        let mut insn_bytecode_location = Vec::new();
        for (language, dir) in config.iter_original_facts() {
            if language != DetectLanguage::Pcode {
                continue;
            }
            // this code reads all the facts for all instructions but returns only those which are
            // tainted. we could filter on load; we don't, not because it is impossible, but because it
            // hasn't yet been necessary.
            let facts_uri = Uri(path::PathBuf::from(dir)
                .clone()
                .into_os_string()
                .into_string()
                .unwrap()
                .into());
                
            insn_bytecode_location.extend(
                read_csv::<(Str, i64)>(&dir.join("PCODE_TARGET.facts"))?
                    .map(|(i, off)| {
                        (
                            Insn(i),
                            Uri(Str::from("ghidra")),
                            ByteOffset(off),
                        )
                    })
                    .sorted()
                    .dedup(),
            );

        }
        let prog = ascent::ascent_run! {
            // Facts:

            relation tainted_insn(Insn) = tainted_insn.into_iter().map(|i| (i.clone(),)).collect();
            relation insn_bytecode_location(Insn, Uri, ByteOffset) = insn_bytecode_location;

            // Derived:

            // Insn is tainted with label bc of var and path
            relation insn_source_pointer(Insn, ArtifactId, RegionId);
            relation insn_source_location(Insn, Uri, Region, Uri, Region);

            insn_source_location(insn, uri, region.clone(), uri, region.clone()) <--
                tainted_insn(insn),
                insn_bytecode_location(insn, uri, byte_offset),
                let region = RegionBuilder::new().addr(insn.0.to_string(), Some(byte_offset.0)).build();
        };
        Ok(prog
            .insn_source_location
            .into_iter()
            .map(|(i, uri, region, buri, bregion)| (i, uri, region, Some(buri), Some(bregion)))
            .collect())
    }
}

lazy_static::lazy_static! {
    static ref GLOBAL: Str = Str::from("<globals>");
    static ref STAR: Str = Str::from(".*");
    static ref EMPTY: Str = Str::from("");
    static ref TRUE: Str = Str::from("true");
    static ref FALSE: Str = Str::from("false");
    static ref NAME: Str = Str::from("name");
}
static PCODE_RET_ARG_INDEX: i64 = -1;
static PCODE_FUNC_ARG_INDEX: i64 = -2;

macro_rules! read_all_facts {
    ($dir:expr, $(($name:expr, $obj:expr)),*) => {
        $(
            dbg!($name);
            $obj.read_facts(std::path::PathBuf::from($dir.join($name))).unwrap();
        )*
    };
}

trait FactStorage {
    /// Reads facts from the path into storage
    fn read_facts(&self, p: path::PathBuf) -> anyhow::Result<()>;
}

impl<T> FactStorage for Arc<Mutex<Vec<T>>>
where
    T: serde::Serialize + Default + std::fmt::Debug + Ord,
    for<'de> T: serde::de::Deserialize<'de>,
{
    fn read_facts(&self, p: path::PathBuf) -> anyhow::Result<()> {
        let facts = read_csv::<T>(p.as_path())
            .with_context(|| {
                format!(
                    "Failed attempting to read original program facts: {}",
                    p.display()
                )
            })?
            .collect::<Vec<_>>();
        let mut data = self.lock().unwrap();
        data.reserve(facts.len());
        data.extend(facts);
        data.sort();
        data.dedup();
        Ok(())
    }
}

/// Use JADX jar to import from an apk/jar file
///
/*
pub fn jadx_import<P>(outdir: P, filename: String, args: Vec<String>) -> anyhow::Result<()>
where
    P: AsRef<path::Path> + std::marker::Sync,
{
    // Look for the import jar relative to the ./target/debug/<binary> path
    let exe_path = std::env::current_exe()?;
    let resources_path = exe_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ctadl-ascent")
        .join("resources");
    let jar_path = resources_path.join("ctadl-jadx-import.jar");
    if !jar_path.exists() {
        anyhow::bail!("JADX import jar doesn't exist: {}", jar_path.display());
    }
    //eprintln!("resources path: {}", resources_path.display());
    // Prefer JAVA_HOME, fall back to 'java'
    let mut command = Command::new({
        match std::env::var("JAVA_HOME") {
            Ok(java_home) => {
                // Construct the path to the Java executable
                let java_executable = path::Path::new(&java_home).join("bin").join("java");
                java_executable.into_os_string()
            }
            Err(_) => "java".into(),
        }
    });

    command.arg("-jar").arg(jar_path.into_os_string());
    command.arg(filename);
    command.arg("--output").arg(outdir.as_ref());
    command.args(args);
    let status = command
        .status()
        .with_context(|| format!("Failed to execute java importer: {command:?}"))?;
    if status.success() {
        println!("Import executed successfully");
    } else {
        eprintln!("Import failed with exit code: {}", status);
    }
    let mut command2 = Command::new("python3");
    command2.arg(
        resources_path
            .join("process_source_maps.py")
            .into_os_string(),
    );
    command2.arg(outdir.as_ref());
    let status2 = command2
        .status()
        .with_context(|| "Failed to execute process_source_maps")?;
    if status2.success() {
        println!("Post-processing executed successfully");
    } else {
        eprintln!("Post-processing failed with exit code: {}", status);
    }
    Ok(())
}
*/

/// Convert fact dirs from pcode into our internal representation.
pub fn pcode_convert(dirs: Vec<path::PathBuf>) -> anyhow::Result<ir::ConvertResult> {
    println!("HIMOM pcode_convert");
    let now = Instant::now();
    let program_file = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    
    let hfunc_func = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hfunc_tostr = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hfunc_proto = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hfunc_ep = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let hfunc_local_ep = Arc::new(Mutex::new(Vec::<(i64, i64)>::new()));
    let hfunc_isext = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let hfunc_cspec = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hfunc_lang = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hfunc_name = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    
    let hvar_name = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hvar_size = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hvar_class = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hvar_scope = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hvar_type = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let hvar_representative = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    
    let pcode_tostr = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let pcode_mnemonic = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let pcode_opcode = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let pcode_parent = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let pcode_input_count = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let pcode_input = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let pcode_output = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let pcode_next = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let pcode_target = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let pcode_time = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let pcode_index = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));

    let vnode_address = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let vnode_is_address = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let vnode_is_addrtied = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let vnode_pc_address = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let vnode_desc = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_name = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_offset = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_offset_n = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let vnode_size = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let vnode_space = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_tostr = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_hvar = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_hfunc = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vnode_def = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));

    let offset_index = Arc::new(Mutex::new(Vec::<(i64, i64)>::new()));

    let type_name = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let type_length = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let type_pointer = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_pointer_base = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let type_array = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_array_base = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let type_array_n = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let type_array_element_length = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let type_struct = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_struct_field = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_struct_offset = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_struct_offset_n = Arc::new(Mutex::new(Vec::<(Str, i64, i64)>::new()));
    let type_struct_field_name = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_struct_field_name_by_offset = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_struct_field_count = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let type_union = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_union_field = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_union_offset = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_union_offset_n = Arc::new(Mutex::new(Vec::<(Str, i64, i64)>::new()));
    let type_union_field_name = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_union_field_name_by_offset = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_union_field_count = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let type_func = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_func_ret = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let type_func_varargs = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_func_param_count = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let type_func_param = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let type_boolean = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_integer = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_float = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let type_enum = Arc::new(Mutex::new(Vec::<(Str, )>::new()));

    let bb_in = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_out = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_fout = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_tout = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_first = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_last = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_hfunc = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let bb_start = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));

    let proto_is_constructor = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let proto_is_destructor = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let proto_is_vararg = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let proto_is_inline = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let proto_is_void = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let proto_has_this = Arc::new(Mutex::new(Vec::<(Str, )>::new()));
    let proto_calling_convention = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let proto_rettype = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let proto_parameter = Arc::new(Mutex::new(Vec::<(Str, i64, Str)>::new()));
    let proto_parameter_count = Arc::new(Mutex::new(Vec::<(Str, i64)>::new()));
    let proto_parameter_datatype = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));

    let symbol_hvar = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let symbol_hfunc = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let symbol_name = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));

    let data_string = Arc::new(Mutex::new(Vec::<(Str, Str)>::new()));
    let vtable = Arc::new(Mutex::new(Vec::<(Str, Str, i64, Str)>::new()));
    
    // PCODE Mnemonics
    // BOOL_AND
    // BOOL_NEGATE
    // BOOL_OR
    // BRANCH
    // BRANCHIND
    // CALL
    // CALLIND
    // CAST
    // CBRANCH
    // COPY
    // INDIRECT
    // INT_2COMP
    // INT_ADD
    // INT_AND
    // INT_CARRY
    // INT_DIV
    // INT_EQUAL
    // INT_LEFT
    // INT_LESS
    // INT_LESSEQUAL
    // INT_MULT
    // INT_NEGATE
    // INT_NOTEQUAL
    // INT_OR
    // INT_REM
    // INT_RIGHT
    // INT_SCARRY
    // INT_SDIV
    // INT_SEXT
    // INT_SLESS
    // INT_SLESSEQUAL
    // INT_SREM
    // INT_SRIGHT
    // INT_SUB
    // INT_XOR
    // INT_ZEXT
    // LOAD
    // MULTIEQUAL
    // PIECE
    // POPCOUNT
    // PTRADD
    // PTRSUB
    // RETURN
    // STORE
    // SUBPIECE

    dirs.par_iter().for_each(|dir| {
        read_all_facts!(
            dir,
            ("PROGRAM_FILE.facts", &program_file),
    
            ("HFUNC_FUNC.facts", &hfunc_func),
            ("HFUNC_TOSTR.facts", &hfunc_tostr),
            ("HFUNC_PROTO.facts", &hfunc_proto),
            ("HFUNC_EP.facts", &hfunc_ep),
            ("HFUNC_LOCAL_EP.facts", &hfunc_local_ep),
            ("HFUNC_ISEXT.facts", &hfunc_isext),
            ("HFUNC_CSPEC.facts", &hfunc_cspec),
            ("HFUNC_LANG.facts", &hfunc_lang),
            ("HFUNC_NAME.facts", &hfunc_name),
    
            ("HVAR_NAME.facts", &hvar_name),
            ("HVAR_SIZE.facts", &hvar_size),
            ("HVAR_CLASS.facts", &hvar_class),
            ("HVAR_SCOPE.facts", &hvar_scope),
            ("HVAR_TYPE.facts", &hvar_type),
            ("HVAR_REPRESENTATIVE.facts", &hvar_representative),
    
            ("PCODE_TOSTR.facts", &pcode_tostr),
            ("PCODE_MNEMONIC.facts", &pcode_mnemonic),
            ("PCODE_OPCODE.facts", &pcode_opcode),
            ("PCODE_PARENT.facts", &pcode_parent),
            ("PCODE_INPUT_COUNT.facts", &pcode_input_count),
            ("PCODE_INPUT.facts", &pcode_input),
            ("PCODE_OUTPUT.facts", &pcode_output),
            ("PCODE_NEXT.facts", &pcode_next),
            ("PCODE_TARGET.facts", &pcode_target),
            ("PCODE_TIME.facts", &pcode_time),
            ("PCODE_INDEX.facts", &pcode_index),

            ("VNODE_ADDRESS.facts", &vnode_address),
            ("VNODE_IS_ADDRESS.facts", &vnode_is_address),
            ("VNODE_IS_ADDRTIED.facts", &vnode_is_addrtied),
            ("VNODE_PC_ADDRESS.facts", &vnode_pc_address),
            ("VNODE_DESC.facts", &vnode_desc),
            ("VNODE_NAME.facts", &vnode_name),
            ("VNODE_OFFSET.facts", &vnode_offset),
            ("VNODE_OFFSET_N.facts", &vnode_offset_n),
            ("VNODE_SIZE.facts", &vnode_size),
            ("VNODE_SPACE.facts", &vnode_space),
            ("VNODE_TOSTR.facts", &vnode_tostr),
            ("VNODE_HVAR.facts", &vnode_hvar),
            ("VNODE_HFUNC.facts", &vnode_hfunc),
            ("VNODE_DEF.facts", &vnode_def),

            ("OFFSET_INDEX.facts", &offset_index),

            ("TYPE_NAME.facts", &type_name),
            ("TYPE_LENGTH.facts", &type_length),
            ("TYPE_POINTER.facts", &type_pointer),
            ("TYPE_POINTER_BASE.facts", &type_pointer_base),
            ("TYPE_ARRAY.facts", &type_array),
            ("TYPE_ARRAY_BASE.facts", &type_array_base),
            ("TYPE_ARRAY_N.facts", &type_array_n),
            ("TYPE_ARRAY_ELEMENT_LENGTH.facts", &type_array_element_length),
            ("TYPE_STRUCT.facts", &type_struct),
            ("TYPE_STRUCT_FIELD.facts", &type_struct_field),
            ("TYPE_STRUCT_OFFSET.facts", &type_struct_offset),
            ("TYPE_STRUCT_OFFSET_N.facts", &type_struct_offset_n),
            ("TYPE_STRUCT_FIELD_NAME.facts", &type_struct_field_name),
            ("TYPE_STRUCT_FIELD_NAME_BY_OFFSET.facts", &type_struct_field_name_by_offset),
            ("TYPE_STRUCT_FIELD_COUNT.facts", &type_struct_field_count),
            ("TYPE_UNION.facts", &type_union),
            ("TYPE_UNION_FIELD.facts", &type_union_field),
            ("TYPE_UNION_OFFSET.facts", &type_union_offset),
            ("TYPE_UNION_OFFSET_N.facts", &type_union_offset_n),
            ("TYPE_UNION_FIELD_NAME.facts", &type_union_field_name),
            ("TYPE_UNION_FIELD_NAME_BY_OFFSET.facts", &type_union_field_name_by_offset),
            ("TYPE_UNION_FIELD_COUNT.facts", &type_union_field_count),
            ("TYPE_FUNC.facts", &type_func),
            ("TYPE_FUNC_RET.facts", &type_func_ret),
            ("TYPE_FUNC_VARARGS.facts", &type_func_varargs),
            ("TYPE_FUNC_PARAM_COUNT.facts", &type_func_param_count),
            ("TYPE_FUNC_PARAM.facts", &type_func_param),
            ("TYPE_BOOLEAN.facts", &type_boolean),
            ("TYPE_INTEGER.facts", &type_integer),
            ("TYPE_FLOAT.facts", &type_float),
            ("TYPE_ENUM.facts", &type_enum),

            ("BB_IN.facts", &bb_in),
            ("BB_OUT.facts", &bb_out),
            ("BB_FOUT.facts", &bb_fout),
            ("BB_TOUT.facts", &bb_tout),
            ("BB_FIRST.facts", &bb_first),
            ("BB_LAST.facts", &bb_last),
            ("BB_HFUNC.facts", &bb_hfunc),
            ("BB_START.facts", &bb_start),

            ("PROTO_IS_CONSTRUCTOR.facts", &proto_is_constructor),
            ("PROTO_IS_DESTRUCTOR.facts", &proto_is_destructor),
            ("PROTO_IS_VARARG.facts", &proto_is_vararg),
            ("PROTO_IS_INLINE.facts", &proto_is_inline),
            ("PROTO_IS_VOID.facts", &proto_is_void),
            ("PROTO_HAS_THIS.facts", &proto_has_this),
            ("PROTO_CALLING_CONVENTION.facts", &proto_calling_convention),
            ("PROTO_RETTYPE.facts", &proto_rettype),
            ("PROTO_PARAMETER.facts", &proto_parameter),
            ("PROTO_PARAMETER_COUNT.facts", &proto_parameter_count),
            ("PROTO_PARAMETER_DATATYPE.facts", &proto_parameter_datatype),

            ("SYMBOL_HVAR.facts", &symbol_hvar),
            ("SYMBOL_HFUNC.facts", &symbol_hfunc),
            ("SYMBOL_NAME.facts", &symbol_name),

            //("DATA_STRING.facts", &data_string),
            ("VTABLE.facts", &vtable)
        );
    });

    let program_file = Arc::try_unwrap(program_file).unwrap().into_inner().unwrap();
    
    let hfunc_func = Arc::try_unwrap(hfunc_func).unwrap().into_inner().unwrap();
    let hfunc_tostr = Arc::try_unwrap(hfunc_tostr).unwrap().into_inner().unwrap();
    let hfunc_proto = Arc::try_unwrap(hfunc_proto).unwrap().into_inner().unwrap();
    let hfunc_ep = Arc::try_unwrap(hfunc_ep).unwrap().into_inner().unwrap();
    let hfunc_local_ep = Arc::try_unwrap(hfunc_local_ep).unwrap().into_inner().unwrap();
    let hfunc_isext = Arc::try_unwrap(hfunc_isext).unwrap().into_inner().unwrap();
    let hfunc_cspec = Arc::try_unwrap(hfunc_cspec).unwrap().into_inner().unwrap();
    let hfunc_lang = Arc::try_unwrap(hfunc_lang).unwrap().into_inner().unwrap();
    let hfunc_name = Arc::try_unwrap(hfunc_name).unwrap().into_inner().unwrap();
    
    let hvar_name = Arc::try_unwrap(hvar_name).unwrap().into_inner().unwrap();
    let hvar_size = Arc::try_unwrap(hvar_size).unwrap().into_inner().unwrap();
    let hvar_class = Arc::try_unwrap(hvar_class).unwrap().into_inner().unwrap();
    let hvar_scope = Arc::try_unwrap(hvar_scope).unwrap().into_inner().unwrap();
    let hvar_type = Arc::try_unwrap(hvar_type).unwrap().into_inner().unwrap();
    let hvar_representative = Arc::try_unwrap(hvar_representative).unwrap().into_inner().unwrap();
    
    let pcode_tostr = Arc::try_unwrap(pcode_tostr).unwrap().into_inner().unwrap();
    let pcode_mnemonic = Arc::try_unwrap(pcode_mnemonic).unwrap().into_inner().unwrap();
    let pcode_opcode = Arc::try_unwrap(pcode_opcode).unwrap().into_inner().unwrap();
    let pcode_parent = Arc::try_unwrap(pcode_parent).unwrap().into_inner().unwrap();
    let pcode_input_count = Arc::try_unwrap(pcode_input_count).unwrap().into_inner().unwrap();
    let pcode_input = Arc::try_unwrap(pcode_input).unwrap().into_inner().unwrap();
    let pcode_output = Arc::try_unwrap(pcode_output).unwrap().into_inner().unwrap();
    let pcode_next = Arc::try_unwrap(pcode_next).unwrap().into_inner().unwrap();
    let pcode_target = Arc::try_unwrap(pcode_target).unwrap().into_inner().unwrap();
    let pcode_time = Arc::try_unwrap(pcode_time).unwrap().into_inner().unwrap();
    let pcode_index = Arc::try_unwrap(pcode_index).unwrap().into_inner().unwrap();

    let vnode_address = Arc::try_unwrap(vnode_address).unwrap().into_inner().unwrap();
    let vnode_is_address = Arc::try_unwrap(vnode_is_address).unwrap().into_inner().unwrap();
    let vnode_is_addrtied = Arc::try_unwrap(vnode_is_addrtied).unwrap().into_inner().unwrap();
    let vnode_pc_address = Arc::try_unwrap(vnode_pc_address).unwrap().into_inner().unwrap();
    let vnode_desc = Arc::try_unwrap(vnode_desc).unwrap().into_inner().unwrap();
    let vnode_name = Arc::try_unwrap(vnode_name).unwrap().into_inner().unwrap();
    let vnode_offset = Arc::try_unwrap(vnode_offset).unwrap().into_inner().unwrap();
    let vnode_offset_n = Arc::try_unwrap(vnode_offset_n).unwrap().into_inner().unwrap();
    let vnode_size = Arc::try_unwrap(vnode_size).unwrap().into_inner().unwrap();
    let vnode_space = Arc::try_unwrap(vnode_space).unwrap().into_inner().unwrap();
    let vnode_tostr = Arc::try_unwrap(vnode_tostr).unwrap().into_inner().unwrap();
    let vnode_hvar = Arc::try_unwrap(vnode_hvar).unwrap().into_inner().unwrap();
    let vnode_hfunc = Arc::try_unwrap(vnode_hfunc).unwrap().into_inner().unwrap();
    let vnode_def = Arc::try_unwrap(vnode_def).unwrap().into_inner().unwrap();

    let offset_index = Arc::try_unwrap(offset_index).unwrap().into_inner().unwrap();

    let type_name = Arc::try_unwrap(type_name).unwrap().into_inner().unwrap();
    let type_length = Arc::try_unwrap(type_length).unwrap().into_inner().unwrap();
    let type_pointer = Arc::try_unwrap(type_pointer).unwrap().into_inner().unwrap();
    let type_pointer_base = Arc::try_unwrap(type_pointer_base).unwrap().into_inner().unwrap();
    let type_array = Arc::try_unwrap(type_array).unwrap().into_inner().unwrap();
    let type_array_base = Arc::try_unwrap(type_array_base).unwrap().into_inner().unwrap();
    let type_array_n = Arc::try_unwrap(type_array_n).unwrap().into_inner().unwrap();
    let type_array_element_length = Arc::try_unwrap(type_array_element_length).unwrap().into_inner().unwrap();
    let type_struct = Arc::try_unwrap(type_struct).unwrap().into_inner().unwrap();
    let type_struct_field = Arc::try_unwrap(type_struct_field).unwrap().into_inner().unwrap();
    let type_struct_offset = Arc::try_unwrap(type_struct_offset).unwrap().into_inner().unwrap();
    let type_struct_offset_n = Arc::try_unwrap(type_struct_offset_n).unwrap().into_inner().unwrap();
    let type_struct_field_name = Arc::try_unwrap(type_struct_field_name).unwrap().into_inner().unwrap();
    let type_struct_field_name_by_offset = Arc::try_unwrap(type_struct_field_name_by_offset).unwrap().into_inner().unwrap();
    let type_struct_field_count = Arc::try_unwrap(type_struct_field_count).unwrap().into_inner().unwrap();
    let type_union = Arc::try_unwrap(type_union).unwrap().into_inner().unwrap();
    let type_union_field = Arc::try_unwrap(type_union_field).unwrap().into_inner().unwrap();
    let type_union_offset = Arc::try_unwrap(type_union_offset).unwrap().into_inner().unwrap();
    let type_union_offset_n = Arc::try_unwrap(type_union_offset_n).unwrap().into_inner().unwrap();
    let type_union_field_name = Arc::try_unwrap(type_union_field_name).unwrap().into_inner().unwrap();
    let type_union_field_name_by_offset = Arc::try_unwrap(type_union_field_name_by_offset).unwrap().into_inner().unwrap();
    let type_union_field_count = Arc::try_unwrap(type_union_field_count).unwrap().into_inner().unwrap();
    let type_func = Arc::try_unwrap(type_func).unwrap().into_inner().unwrap();
    let type_func_ret = Arc::try_unwrap(type_func_ret).unwrap().into_inner().unwrap();
    let type_func_varargs = Arc::try_unwrap(type_func_varargs).unwrap().into_inner().unwrap();
    let type_func_param_count = Arc::try_unwrap(type_func_param_count).unwrap().into_inner().unwrap();
    let type_func_param = Arc::try_unwrap(type_func_param).unwrap().into_inner().unwrap();
    let type_boolean = Arc::try_unwrap(type_boolean).unwrap().into_inner().unwrap();
    let type_integer = Arc::try_unwrap(type_integer).unwrap().into_inner().unwrap();
    let type_float = Arc::try_unwrap(type_float).unwrap().into_inner().unwrap();
    let type_enum = Arc::try_unwrap(type_enum).unwrap().into_inner().unwrap();

    let bb_in = Arc::try_unwrap(bb_in).unwrap().into_inner().unwrap();
    let bb_out = Arc::try_unwrap(bb_out).unwrap().into_inner().unwrap();
    let bb_fout = Arc::try_unwrap(bb_fout).unwrap().into_inner().unwrap();
    let bb_tout = Arc::try_unwrap(bb_tout).unwrap().into_inner().unwrap();
    let bb_first = Arc::try_unwrap(bb_first).unwrap().into_inner().unwrap();
    let bb_last = Arc::try_unwrap(bb_last).unwrap().into_inner().unwrap();
    let bb_hfunc = Arc::try_unwrap(bb_hfunc).unwrap().into_inner().unwrap();
    let bb_start = Arc::try_unwrap(bb_start).unwrap().into_inner().unwrap();

    let proto_is_constructor = Arc::try_unwrap(proto_is_constructor).unwrap().into_inner().unwrap();
    let proto_is_destructor = Arc::try_unwrap(proto_is_destructor).unwrap().into_inner().unwrap();
    let proto_is_vararg = Arc::try_unwrap(proto_is_vararg).unwrap().into_inner().unwrap();
    let proto_is_inline = Arc::try_unwrap(proto_is_inline).unwrap().into_inner().unwrap();
    let proto_is_void = Arc::try_unwrap(proto_is_void).unwrap().into_inner().unwrap();
    let proto_has_this = Arc::try_unwrap(proto_has_this).unwrap().into_inner().unwrap();
    let proto_calling_convention = Arc::try_unwrap(proto_calling_convention).unwrap().into_inner().unwrap();
    let proto_rettype = Arc::try_unwrap(proto_rettype).unwrap().into_inner().unwrap();
    let proto_parameter = Arc::try_unwrap(proto_parameter).unwrap().into_inner().unwrap();
    let proto_parameter_count = Arc::try_unwrap(proto_parameter_count).unwrap().into_inner().unwrap();
    let proto_parameter_datatype = Arc::try_unwrap(proto_parameter_datatype).unwrap().into_inner().unwrap();

    let symbol_hvar = Arc::try_unwrap(symbol_hvar).unwrap().into_inner().unwrap();
    let symbol_hfunc = Arc::try_unwrap(symbol_hfunc).unwrap().into_inner().unwrap();
    let symbol_name = Arc::try_unwrap(symbol_name).unwrap().into_inner().unwrap();

    let data_string = Arc::try_unwrap(data_string).unwrap().into_inner().unwrap();
    let vtable = Arc::try_unwrap(vtable).unwrap().into_inner().unwrap();
    let elapsed = now.elapsed();
    log::info!("{:.2?} reading facts", elapsed);

    {
/*
        // All methods with some original instructions
        let s = method_insn
            .iter()
            .filter_map(|(m, c)| if *c > 0 { Some(m) } else { None })
            .cloned()
            .collect::<HashSet<_>>();
        // s - those with statements
        let s = s
            .difference(
                &stmt_in_method
                    .iter()
                    .map(|t| t.2.clone())
                    .collect::<HashSet<_>>(),
            )
            .cloned()
            .collect::<HashSet<_>>();
        // s - those we know are external
        let s = s
            .difference(
                &external_method
                    .iter()
                    .map(|t| t.0.clone())
                    .collect::<HashSet<_>>(),
            )
            .cloned()
            .collect::<HashSet<_>>();
        // s - interface types
        let s = s
            .difference(
                &interface_type
                    .iter()
                    .map(|t| t.0.clone())
                    .collect::<HashSet<_>>(),
            )
            .cloned()
            .collect::<HashSet<_>>();
        let c = s.len();
        for m in s {
            log::warn!("Method has bytecode but no statements after pcode translation: {m}");
        }
        if c > 0 {
            log::warn!("Found {c} incomplete methods.");
        }
*/
    }

    log::info!("Running rules...");
    let now = Instant::now();
    let result = ascent::ascent_run! {
    
        // Facts:

        relation program_file(Str) = program_file;
    
        relation hfunc_func(Str, Str) = hfunc_func;
        relation hfunc_tostr(Str, Str) = hfunc_tostr;
        relation hfunc_proto(Str, Str) = hfunc_proto;
        relation hfunc_ep(Str, i64) = hfunc_ep;
        relation hfunc_local_ep(i64, i64) = hfunc_local_ep;
        relation hfunc_isext(Str) = hfunc_isext;
        relation hfunc_cspec(Str, Str) = hfunc_cspec;
        relation hfunc_lang(Str, Str) = hfunc_lang;
        relation hfunc_name(Str, Str) = hfunc_name;
    
        relation hvar_name(Str, Str) = hvar_name;
        relation hvar_size(Str, Str) = hvar_size;
        relation hvar_class(Str, Str) = hvar_class;
        relation hvar_scope(Str, Str) = hvar_scope;
        relation hvar_type(Str, Str) = hvar_type;
        relation hvar_representative(Str, Str) = hvar_representative;
    
        relation pcode_tostr(Str, Str) = pcode_tostr;
        relation pcode_mnemonic(Str, Str) = pcode_mnemonic;
        relation pcode_opcode(Str, Str) = pcode_opcode;
        relation pcode_parent(Str, Str) = pcode_parent;
        relation pcode_input_count(Str, i64) = pcode_input_count;
        relation pcode_input(Str, i64, Str) = pcode_input;
        relation pcode_output(Str, Str) = pcode_output;
        relation pcode_next(Str, Str) = pcode_next;
        relation pcode_target(Str, i64) = pcode_target;
        relation pcode_time(Str, i64) = pcode_time;
        relation pcode_index(Str, i64) = pcode_index;

        relation vnode_address(Str, i64) = vnode_address;
        relation vnode_is_address(Str) = vnode_is_address;
        relation vnode_is_addrtied(Str) = vnode_is_addrtied;
        relation vnode_pc_address(Str, i64) = vnode_pc_address;
        relation vnode_desc(Str, Str) = vnode_desc;
        relation vnode_name(Str, Str) = vnode_name;
        relation vnode_offset(Str, Str) = vnode_offset;
        relation vnode_offset_n(Str, i64) = vnode_offset_n;
        relation vnode_size(Str, i64) = vnode_size;
        relation vnode_space(Str, Str) = vnode_space;
        relation vnode_tostr(Str, Str) = vnode_tostr;
        relation vnode_hvar(Str, Str) = vnode_hvar;
        relation vnode_hfunc(Str, Str) = vnode_hfunc;
        relation vnode_def(Str, Str) = vnode_def;

        relation offset_index(i64, i64) = offset_index;

        relation type_name(Str, Str) = type_name;
        relation type_length(Str, i64) = type_length;
        relation type_pointer(Str) = type_pointer;
        relation type_pointer_base(Str, Str) = type_pointer_base;
        relation type_array(Str) = type_array;
        relation type_array_base(Str, Str) = type_array_base;
        relation type_array_n(Str, i64) = type_array_n;
        relation type_array_element_length(Str, i64) = type_array_element_length;
        relation type_struct(Str) = type_struct;
        relation type_struct_field(Str, i64, Str) = type_struct_field;
        relation type_struct_offset(Str, i64, Str) = type_struct_offset;
        relation type_struct_offset_n(Str, i64, i64) = type_struct_offset_n;
        relation type_struct_field_name(Str, i64, Str) = type_struct_field_name;
        relation type_struct_field_name_by_offset(Str, i64, Str) = type_struct_field_name_by_offset;
        relation type_struct_field_count(Str, i64) = type_struct_field_count;
        relation type_union(Str) = type_union;
        relation type_union_field(Str, i64, Str) = type_union_field;
        relation type_union_offset(Str, i64, Str) = type_union_offset;
        relation type_union_offset_n(Str, i64, i64) = type_union_offset_n;
        relation type_union_field_name(Str, i64, Str) = type_union_field_name;
        relation type_union_field_name_by_offset(Str, i64, Str) = type_union_field_name_by_offset;
        relation type_union_field_count(Str, i64) = type_union_field_count;
        relation type_func(Str) = type_func;
        relation type_func_ret(Str, Str) = type_func_ret;
        relation type_func_varargs(Str) = type_func_varargs;
        relation type_func_param_count(Str, i64) = type_func_param_count;
        relation type_func_param(Str, i64, Str) = type_func_param;
        relation type_boolean(Str) = type_boolean;
        relation type_integer(Str) = type_integer;
        relation type_float(Str) = type_float;
        relation type_enum(Str) = type_enum;

        relation bb_in(Str, Str) = bb_in;
        relation bb_out(Str, Str) = bb_out;
        relation bb_fout(Str, Str) = bb_fout;
        relation bb_tout(Str, Str) = bb_tout;
        relation bb_first(Str, Str) = bb_first;
        relation bb_last(Str, Str) = bb_last;
        relation bb_hfunc(Str, Str) = bb_hfunc;
        relation bb_start(Str, Str) = bb_start;

        relation proto_is_constructor(Str) = proto_is_constructor;
        relation proto_is_destructor(Str) = proto_is_destructor;
        relation proto_is_vararg(Str) = proto_is_vararg;
        relation proto_is_inline(Str) = proto_is_inline;
        relation proto_is_void(Str) = proto_is_void;
        relation proto_has_this(Str) = proto_has_this;
        relation proto_calling_convention(Str, Str) = proto_calling_convention;
        relation proto_rettype(Str, Str) = proto_rettype;
        relation proto_parameter(Str, i64, Str) = proto_parameter;
        relation proto_parameter_count(Str, i64) = proto_parameter_count;
        relation proto_parameter_datatype(Str, Str) = proto_parameter_datatype;

        relation symbol_hvar(Str, Str) = symbol_hvar;
        relation symbol_hfunc(Str, Str) = symbol_hfunc;
        relation symbol_name(Str, Str) = symbol_name;

        relation data_string(Str, Str) = data_string;
        relation vtable(Str, Str, i64, Str) = vtable;

        // Intermediate:

	// interface
	relation func_ptr(Str, Str);
	relation assign_func_ptr_instruction(Str, Str, Str);
	relation call_instruction(Str, Str);

	// limitations:
	// don't handle returns without a value
	// no need to handle branch
	// don't handle stores of const val
	// dont handle mem ops with addrs of pointer arithmetic
	// don't handle loads of void *
	// if i0 unhandled, a subsequent load/store/ptradd/ptrsub also unhandled
	// ptrsubs that add we ignore for now
	// ptradd of const without type info we ignore for now
	// load/store of a pointer in pieces we cry about
	// don't warn about load/store defined in other block
	// no need to warn about GEPs we find
	// if all inputs are const, don't handle


	// a source varnode is not the output of any instruction
	// e.g. stack, register, ram
	relation is_source_varnode(Str);	

	// an externally defined varnode is one we assume to be well defined if it is
	// dereference directly
	relation is_externally_defined_varnode(Str);

	// either a PCODE COPY or INDIRECT or MULTIEQUAL
	relation direct_copy(Str, Str);
	// either a DirectCopy or a CAST
	relation maybe_cast_copy(Str, Str);

// UNUSED:
	// COPY or INDIRECT or MULTIEQUAL
//	relation copy_insn(Str);
	// CAST
//	relation cast_insn(Str);

	// i is an effective load af var.ap.
	// It might be a LOAD instruction. or it might be a subpiece of a load
	// corresponding to a known field. See CastLoadSubpiece
	relation load(Str, Str, Str);

	// i is a STORE to var.ap
	// This instruction is at the highest level and is use to translate to CTADL IR
	relation store(Str, Str, Str);

	// i is a GEPInsn instruction that accesses var.ap. the type of the field is field_type.
	relation gep(Str, Str, Str, Str);

	// Parameter from the function prototype
        relation pcode_formal_param(Str, i64, Str, Str);   

	// output is var.*
	relation assign_abstract_pointer_contents(Str, Str);
	
	// We use types to figure out the type and the field/offset being accessed.
	relation gep_insn(Str);
	relation gep_insn_is_nested(Str);
	
	relation cast_load_subpiece(Str);
	relation partial_func_signature(Str, i64, Str);
	relation pcode_index_in_bb(Str, Str, i64);
	relation same_bb(Str, Str, Str);
	
// UNUSED:
//	relation all_inputs_const(Str);
//	relation inputs_const_up_to(Str, i64);
// 	relation is_arith_mnemonic(Str);
// 	relation pointer_arith(Str);
//	relation vnode(Str);
//	relation cinsn_use(Str, Str);
//	relation c_is_access_path(Str);

	relation varnode_type_reaching(Str, Str);
	relation varnode_type_ghidra(Str, Str); 
	
// UNUSED:
//      relation cfunction_arity(Str, i64);       
//      relation cfunction_name(Str, Str);       
//      relation cfunction_signature(Str, Str);       

        relation cfunction_is_formal_param_by_ref(Str, i64);       
        relation indirect_for_call_site(Str, Str);       
        relation cvar_name(Str, Str);      
        relation cvar_in_function(Str, Str);       
        relation cvar_is_global(Str);       
        relation cvar_source_info(Str, Str, Str);
        relation stmt_in_function(Str, Str);
	relation csourceinfo_address(i64);
	relation csourceinfo_location(Str, i64, i64);
 	relation csourceinfo_file(i64, Str);
 	relation caddress_absolute_address(i64, i64);
	relation caddress_fully_qualified_name(i64, Str);
	relation caddress_kind(i64, Str);
	relation caddress_name(i64, Str);
	relation field_access(Str, Str, Str, Str);
	relation is_const_varnode(Str);
	
// UNUSED:
//	relation ccall_virtual_base(Str, i64);
//	relation c_is_alloc(Str, Str, Str);
//	relation c_is_function(Str);
//	relation c_is_namespace(Str);
      
        // Output:

        relation o_formal_param(Str, i64, Str);
	// INDIRECT instructions associated with callsite
        relation o_call_site(Str, Str);
        relation o_actual_param(Str, i64, Str, Str);
        relation o_call(Str, Str);
        relation o_assign(Str, Str, FlowType, Str, Str, Str, Str);
        relation o_paths(Str);
        
       
        // Rules:

	is_source_varnode(vn.clone()) <--
	    vnode_space(vn, _),
	    !pcode_output(_, vn);


	is_externally_defined_varnode(vn.clone()) <--
	    is_source_varnode(vn);

	is_externally_defined_varnode(vn.clone()) <--
	    call_instruction(i, _),
	    pcode_output(i, vn);

	is_externally_defined_varnode(vn.clone()) <--
	    is_externally_defined_varnode(vn0),
	    maybe_cast_copy(vn, vn0);


	direct_copy(vn_to.clone(), vn_from.clone()) <--
	    pcode_mnemonic(i, mn),
	    if mn == "copy" || mn == "multiequal",
	    pcode_input(i, _, vn_from),
	    pcode_output(i, vn_to),
	    if vn_from != vn_to;

	direct_copy(vn_to.clone(), vn_from.clone()) <--
	    pcode_mnemonic(i, mn),
	    if mn == "indirect",
	    pcode_input(i, 0, vn_from),
	    pcode_output(i, vn_to),
	    if vn_from != vn_to;


	maybe_cast_copy(vn_to.clone(), vn_from.clone()) <--
	    direct_copy(vn_to, vn_from);

	maybe_cast_copy(vn_to.clone(), vn_from.clone()) <--
	    pcode_mnemonic(i, Str::from("cast")),
	    pcode_input(i, 0, vn_from),
	    pcode_output(i, vn_to),
	    if vn_from != vn_to;

// UNUSED:
/*
	copy_insn(i.clone()) <--
	    pcode_mnemonic(i, mn),
	    if mn == "copy" || mn == "multiequal" || mn == "indirect";
	cast_insn(i.clone()) <--
	    pcode_mnemonic(i, mn),
	    if mn == "cast";
*/


	// ---------------------------------------------------------------------------
	// pointer constant offsets

	// we want to be able to reason about fields we don't know about in a
	// reasonable way. so if we have:
	//
	// local_addr = ptrsub ebp -44
	// arg = (char *)(local_addr + 1)
	//
	// we want to treat this as arg = local_addr.*

	// output is var.*

	assign_abstract_pointer_contents(i.clone(), base.clone()) <--
	    gep_insn(i),
	    !field_access(i, _, _, _),
	    pcode_input(i, 0, base),
	    pcode_input(i, 1, offset),
	    is_const_varnode(offset),
	    is_externally_defined_varnode(base);

        o_assign(f.clone(), stmt.clone(), FlowType::Direct, out.clone(), EMPTY.clone(), base.clone(), STAR.clone()) <--
 	    stmt_in_function(f, method),
    	    assign_abstract_pointer_contents(stmt, base),
    	    pcode_output(stmt, out);



	// ---------------------------------------------------------------------------
	// field access

	// we use types to figure out the type and the field/offset being accessed.

	gep_insn(i.clone()) <--
	    pcode_mnemonic(i, mn),
	    if mn == "ptrsub" || mn == "ptradd";

	gep_insn_is_nested(i2.clone()) <--
	    gep_insn(i2),
	    pcode_input(i2, 0, i2_in),
	    (maybe_cast_copy(i2_in, i1_out) | let i1_out = i2_in),
   	    pcode_output(i1, i1_out),
	    gep_insn(i1),
	    if i1 != i2,
	    // i1 and i2 are in same bb
	    pcode_index_in_bb(bb, i1, index1),
	    pcode_index_in_bb(bb, i2, index2),
	    // and i1 always executes before i2
	    if index2 > index1;



	gep(i.clone(), structure_ptr.clone(), field_ap.clone(), field_type.clone()) <--
	    field_access(i, structure_ptr, field_ap, field_type),
	    !gep_insn_is_nested(i);

	gep(i1.clone(), base.clone(), field_ap.clone(), field_type.clone()) <--
	    gep(i0, base, ap0, _type0),
	    pcode_output(i0, out0),
	    pcode_input(i1, 0, out0),
	    same_bb(bb, i0, i1),
	    if i0 != i1,
	    field_access(i1, _, ap1, field_type),
	    let field_ap = Str::from(format!("{ap0}{ap1}"));

	// propagate gepinsn through cast
	// if there is a gep output that is then casted, and we know the resultant
	// type of the cast, make a new gepinsn at that type
	gep(i1.clone(), base.clone(), ap.clone(), type1.clone()) <--
	    gep(i0, base, ap, _type0),
	    pcode_output(i0, out0),
	    pcode_input(i1, 0, out0),
	    pcode_mnemonic(i1, Str::from("CAST")),
	    pcode_output(i1, out1),
	    varnode_type_reaching(out1, type1);
	//.plan 1: (6, 5, 4, 3, 2, 1)

	gep(i1.clone(), base.clone(), ap.clone(), type0.clone()) <--
	    gep(i0, base, ap, type0),
	    pcode_output(i0, out0),
	    pcode_input(i1, 0, out0),
	    pcode_mnemonic(i1, Str::from("CAST")),
	    pcode_output(i1, out1),
	    !varnode_type_reaching(out1, _);

	load(i.clone(), var.clone(), ap.clone()) <--
	    is_externally_defined_varnode(addr),
	    pcode_input(i, 1, addr),
	    pcode_mnemonic(i, Str::from("LOAD")),
	    let var = addr,
	    let ap = Str::from(".[0]");

	load(i.clone(), var.clone(), ap.clone()) <--
	    gep(i_gep, var, ap, _field_ty),
	    pcode_output(i_gep, gep_out),
	    pcode_input(i, 1, gep_out),
	    pcode_mnemonic(i, Str::from("LOAD")),
	    pcode_output(i, _out);

	// a load, above, may be used for a subsequent load in the code:
	// r->foo->bar
	// so make a new gep with the appropriate element type so that this translates to
	// r.foo.bar
	gep(i.clone(), var.clone(), ap.clone(), elt_ty.clone()) <--
	    gep(i_gep, var, ap, ptr_ty),
	    pcode_output(i_gep, gep_out),
	    pcode_input(i, 1, gep_out),
	    load(i, var, ap),
	    dereference_type(ptr_ty, elt_ty);

	store(i.clone(), var.clone(), ap.clone()) <--
	    is_externally_defined_varnode(addr),
	    pcode_input(i, 1, addr),
	    pcode_mnemonic(i, Str::from("STORE")),
	    let var = addr,
	    let ap = Str::from(".[0]");

	store(i.clone(), var.clone(), ap.clone()) <--
	    gep(i_gep, var, ap, _),
	    pcode_output(i_gep, gep_out),
	    pcode_input(i, 1, gep_out),
	    pcode_mnemonic(i, Str::from("STORE"));


	load(i2.clone(), var.clone(), field.clone()),
	cast_load_subpiece(i_m1.clone()) <--
	    gep(i_m1, var, field, field_ty),
	    pcode_output(i_m1, gep_out),
	    pcode_input(i0, 0, gep_out),
	    pcode_mnemonic(i0, Str::from("CAST")),
	    pcode_output(i0, cast_out),
	    pcode_input(i1, 1, cast_out),
	    pcode_mnemonic(i1, Str::from("LOAD")),
	    pcode_output(i1, load_out),
	    pcode_input(i2, 0, load_out),
	    pcode_mnemonic(i2, Str::from("SUBPIECE")),
	    pcode_output(i2, subpiece_out),
	    type_length(field_ty, field_size),
	    vnode_size(subpiece_out, subpiece_out_size),
	    if field_size == subpiece_out_size;



        pcode_formal_param(f.clone(), n, v.clone(), name.clone()) <--
    	    hfunc_proto(f, p),
    	    proto_parameter(p, n, hs),
    	    symbol_hvar(hs, hv),
    	    hvar_name(hv, name),
    	    hvar_representative(hv, v);

// UNUSED:
/*
	c_is_access_path(STAR.clone());

	c_is_access_path(ap.clone()),
	cinsn_use(vn.clone(), ap.clone()) <--
	    (load(_, vn, ap) |
	    store(_, vn, ap) |
	    field_access(_, vn, ap, _));
*/

	// ---------------------------------------------------------------------------
	// vars


	indirect_for_call_site(insn.clone(), indirect_insn.clone()) <--
    	    call_instruction(insn, _),
    	    pcode_target(insn, target),
    	    pcode_target(indirect_insn, target),
    	    pcode_mnemonic(indirect_insn, Str::from("INDIRECT"));
    	    
    	    
	// Creates a formal threaded globals parameter for each global offset
	cvar_name(var.clone(), Str::from(format!("ram@{offset}"))),
	cvar_in_function(var.clone(), func.clone()),
	cfunction_is_formal_param_by_ref(func.clone(), -offset),
	o_formal_param(func.clone(), -offset, var.clone()) <--
    	    vnode_space(global_vn, Str::from("ram")),
    	    vnode_offset_n(global_vn, offset),
    	    offset_index(offset, offset),
    	    vnode_hfunc(global_vn, func),
    	    let var = Str::from(format!("{func}:global#{offset}"));
    	    
	// Each indirect global varnode that is associated with the call site needs to
	// be passed as a special threaded globals parameter
        o_actual_param(stmt.clone(), pos, global_param.clone(), EMPTY.clone()) <--
    	    indirect_for_call_site(stmt, indirect_stmt),
    	    pcode_input(indirect_stmt, 0, global_vn),
     	    vnode_space(global_vn, Str::from("ram")),
    	    vnode_offset_n(global_vn, offset),
    	    offset_index(offset, pos),
    	    let global_param = global_vn;
    	    
	cvar_in_function(var.clone(), func.clone()) <--
     	    vnode_hfunc(var, func);

	// ---------------------------------------------------------------------------
	// functions

// UNUSED:
/*
	c_is_function(f) <-- hfunc_func(f, _);
	cfunction_name(f, n) <-- hfunc_name(f, n);
	c_is_namespace(func) <-- c_is_function(func);
*/

	cvar_source_info(v.clone(), NAME.clone(), name.clone()) <--
    	    vnode_hvar(v, hv),
    	    hvar_name(hv, name);

	cvar_source_info(vn.clone(), NAME.clone(), name.clone()) <--
    	    pcode_mnemonic(i, Str::from("PTRSUB")),
    	    pcode_output(i, vn),
    	    (   pcode_input(i, 0, zero), pcode_input(i, 1, inp)
    	    |   pcode_input(i, 1, zero), pcode_input(i, 0, inp)),
    	    is_const_varnode(zero),
    	    vnode_offset_n(zero, 0),
    	    vnode_hvar(inp, hv),
    	    hvar_name(hv, name);

	cvar_source_info(vn.clone(), NAME.clone(), name.clone()) <--
    	    pcode_mnemonic(i, Str::from("PTRSUB")),
    	    pcode_output(i, vn),
    	    (   pcode_input(i, 0, zero), pcode_input(i, 1, inp)
    	    |   pcode_input(i, 1, zero), pcode_input(i, 0, inp)),
    	    is_const_varnode(zero),
    	    vnode_offset_n(zero, 0),
    	    vnode_hvar(inp, hv),
    	    symbol_hvar(sym, hv),
    	    symbol_name(sym, name);
   
    	cvar_name(v.clone(), name.clone()) <--
     	    vnode_name(v, name);
	
	// Ghidra sometimes makes registers into formals. But we are treating registers
	// as global. So create fresh parameter names for each formal and copy them to
	// the parameter varnode
	csourceinfo_address(var_addr_id.clone()),
	csourceinfo_address(insn_addr_id.clone()),
	caddress_absolute_address(var_addr_id.clone(), func_addr),
	caddress_absolute_address(insn_addr_id.clone(), func_addr),
	caddress_kind(var_addr_id.clone(), Str::from("data")),
	caddress_kind(insn_addr_id.clone(), Str::from("instruction")),
	csourceinfo_location(param.clone(), 1, var_addr_id.clone()),
	csourceinfo_location(move_insn.clone(), 1, insn_addr_id.clone()),
	
	cvar_name(param.clone(), param_name.clone()),
	cvar_source_info(param.clone(), NAME.clone(), param_name.clone()),
	stmt_in_function(move_insn.clone(), f.clone()),
	o_assign(f.clone(), move_insn.clone(), FlowType::Direct, v, "".into(), param.clone(), "".into()),
	cvar_in_function(param.clone(), f.clone()),
	o_formal_param(f.clone(), n, param.clone()) <--
    	    pcode_formal_param(f, n, v, param_name),
    	    !hfunc_isext(f),
    	    let param = Str::from(format!("{f}:@{param_name}")),
    	    let move_insn = Str::from(format!("{f}!copy_formal")),
    	    hfunc_ep(f, ep),
    	    hfunc_local_ep(ep, func_addr),
    	    let var_addr_id = param.as_ptr() as usize as i64,
    	    let insn_addr_id = move_insn.as_ptr() as usize as i64;

// UNUSED:
/*
	// only 1 file
	csourceinfo_file(1, file) <--
	// using absolute paths for ghidra atm, so don't use uribaseid
	//cfile_uribaseid(1, "binroot") <--
	    program_file(file);
*/

	csourceinfo_location(param.clone(), 1, region_id),
	csourceinfo_address(region_id),
	caddress_absolute_address(region_id, func_addr.clone()),
	caddress_kind(region_id, Str::from("data")),
	cvar_name(param.clone(), param_name.clone()),
	cvar_source_info(param.clone(), NAME.clone(), param_name.clone()),
	cvar_in_function(param.clone(), f.clone()),
	o_formal_param(f.clone(), n, param.clone()) <--
    	    pcode_formal_param(f, n, _, param_name),
    	    hfunc_isext(f),
     	    let param = Str::from(format!("{f}:@{param_name}")),
     	    hfunc_ep(f, ep),
    	    hfunc_local_ep(ep, func_addr),
    	    let region_id = param.as_ptr() as usize as i64;

	// return param
	csourceinfo_location(retparam.clone(), 1, region_id),
	csourceinfo_address(region_id),
	caddress_absolute_address(region_id, addr.clone()),
	caddress_kind(region_id, Str::from("data")),
	cvar_name(retparam.clone(), Str::from("@ret")),
	cvar_in_function(retparam.clone(), f.clone()),
	cfunction_is_formal_param_by_ref(f.clone(), -1),
	o_formal_param(f.clone(), pos, retparam.clone()) <--
    	    hfunc_func(f, _),
	    hfunc_ep(f, ep_addr),
	    hfunc_local_ep(ep_addr, addr),
	    let retparam = Str::from(format!("{f}:@ret")),
   	    let region_id = retparam.as_ptr() as usize as i64,
    	    let pos = PCODE_RET_ARG_INDEX;

	// copy return'd varnode into return param
	stmt_in_function(i.clone(), f.clone()),
	o_assign(f.clone(), i.clone(), FlowType::Direct, retparam.clone(), EMPTY.clone(), vn.clone(), EMPTY.clone()) <--
    	    pcode_mnemonic(i, Str::from("RETURN")),
    	    pcode_input(i, 1, vn),
    	    pcode_parent(i, bb),
    	    bb_hfunc(bb, f),
	    let retparam = Str::from(format!("{f}:@ret")),
    	    pcode_index(i, index);
    	    
// UNUSED:
/*
	cfunction_arity(f, n) <--
	    hfunc_proto(f, p),
	    proto_parameter_count(p, n);
*/

	// call it byref if type is pointer
	cfunction_is_formal_param_by_ref(f.clone(), n) <--
    	    pcode_formal_param(f, n, v, _),
    	    hvar_representative(h, v),
    	    hvar_type(h, t),
    	    type_pointer(t);

	stmt_in_function(i.clone(), f.clone()) <--
	    (o_assign(f, i, FlowType::Direct, _, _, _, _) | call_instruction(i, _)),
	    pcode_parent(i, bb),
	    bb_hfunc(bb, f);

	partial_func_signature(id.clone(), 0, sig.clone()) <--
	    proto_parameter(id, 0, hs),
	    proto_parameter_datatype(hs, ttype),
	    let sig = ttype;

	partial_func_signature(id, n+1, sig2) <--
	    partial_func_signature(id, n, sig),
	    proto_parameter(id, n+1, hs),
	    proto_parameter_datatype(hs, ttype),
	    let sig2 = Str::from(format!("{sig},{ttype}"));

// UNUSED:
/*
	cfunction_signature(func, sig) <--
	    hfunc_proto(func, proto_id),
	    proto_parameter_count(proto_id, n),
	    partial_func_signature(proto_id, n-1, proto_sig),
	    (   (proto_is_void(proto_id), let rettype = Str::from("void"))
	    |   (!proto_is_void(proto_id), proto_rettype(proto_id, rettype))),
	    (   (proto_is_vararg(proto_id), let varargs = Str::from(",..."))
	    |   (!proto_is_vararg(proto_id), let varargs = Str::from(""))),
	    (   (proto_is_inline(proto_id), let inln = Str::from("inline "))
	    |   (!proto_is_inline(proto_id), let inln = Str::from(""))),
	    (   (proto_is_constructor(proto_id), let constructor = Str::from(" constructor"))
	    |   (!proto_is_constructor(proto_id), let constructor = Str::from(""))),
	    (   (proto_is_destructor(proto_id), let destructor = Str::from(" destructor"))
	    |   (!proto_is_destructor(proto_id), let destructor = Str::from(""))),
	    (   (proto_has_this(proto_id), let has_this = Str::from(" has_this"))
	    |   (!proto_has_this(proto_id), let has_this = Str::from(""))),
	    let sig = Str::from(format!("{inln}{rettype}({proto_sig}{varargs}){constructor}{destructor}{has_this}")); 
*/

	// ---------------------------------------------------------------------------
	// calls
	
	o_call_site(i.clone(), f.clone()) <--
	    call_instruction(i, _),
	    pcode_parent(i, bb),
	    bb_hfunc(bb, f);    
 
 	// direct call
	o_call(i.clone(), f.clone()) <--
    	    call_instruction(i, func_op),
    	    vnode_address(func_op, a),
    	    (   hfunc_local_ep(ep, a), hfunc_ep(f, ep)
    	    |   hfunc_ep(f, a));

       o_actual_param(stmt.clone(), pos, v.clone(), EMPTY.clone()) <--
            call_instruction(stmt, _),
            pcode_output(stmt, v),
            let pos = PCODE_RET_ARG_INDEX;

        o_actual_param(stmt.clone(), n-1, v.clone(), EMPTY.clone()) <--
            call_instruction(stmt, _),
            pcode_input(stmt, n, v),
            if *n > 0; // skip input 0, it's not a function actual parameter

        o_actual_param(stmt.clone(), pos, v.clone(), EMPTY.clone()) <--
// UNUSED:
//     ccall_virtual_base(stmt.clone(), pos) <--
    	    pcode_mnemonic(stmt, Str::from("CALLIND")),
            pcode_input(stmt, 0, v),
            let pos = PCODE_FUNC_ARG_INDEX;
            
	// ---------------------------------------------------------------------------
	// moves
            
	o_assign(f.clone(), i.clone(), FlowType::Direct, v_to.clone(), EMPTY.clone(), v_from.clone(), ap_from.clone()) <--
 	    stmt_in_function(i, f),
    	    load(i, v_from, ap_from),
    	    pcode_output(i, v_to);
    	    
	o_assign(f.clone(), i.clone(), FlowType::Direct, v_to.clone(), ap_to.clone(), v_from.clone(), EMPTY.clone()) <--
 	    stmt_in_function(i, f),
    	    store(i, v_to, ap_to),
    	    pcode_input(i, 2, v_from);

	o_assign(f.clone(), i.clone(), FlowType::Direct, v_to.clone(), EMPTY.clone(), v_from.clone(), EMPTY.clone()) <--
 	    stmt_in_function(i, f),
    	    pcode_mnemonic(i, mnemonic),
	    if mnemonic == "INT_ADD" ||
	       mnemonic == "INT_AND" ||
	       mnemonic == "INT_SRIGHT" ||
	       mnemonic == "INT_RIGHT" ||
	       mnemonic == "INT_MULT" ||
	       mnemonic == "INT_OR" ||
	       mnemonic == "INT_SDIV" ||
	       mnemonic == "INT_LEFT" ||
	       mnemonic == "INT_SREM" ||
	       mnemonic == "INT_SUB" ||
	       mnemonic == "INT_DIV" ||
	       mnemonic == "INT_REM" ||
	       mnemonic == "INT_XOR" ||
	       mnemonic == "INT_CARRY" ||
	       mnemonic == "INT_SCARRY" ||
	       mnemonic == "INT_SBORROW" ||
	       mnemonic == "FLOAT_ADD" ||
	       mnemonic == "FLOAT_DIV" ||
	       mnemonic == "FLOAT_MULT" ||
	       mnemonic == "FLOAT_SUB" ||
	       mnemonic == "BOOL_AND" ||
	       mnemonic == "BOOL_OR" ||
	       mnemonic == "BOOL_XOR" ||
	       mnemonic == "COPY" ||
	       mnemonic == "CAST" ||
	       mnemonic == "MULTIEQUAL" ||
	       mnemonic == "TRUNC" ||
	       mnemonic == "INT_SEXT" ||
	       mnemonic == "INT_ZEXT" ||
	       mnemonic == "INT2FLOAT" ||
	       mnemonic == "INT_2COMP" ||
	       mnemonic == "INT_NEGATE" ||
	       mnemonic == "INT_NOTEQUAL" ||
	       mnemonic == "INT_EQUAL" ||
	       mnemonic == "INT_SLESSEQUAL" ||
	       mnemonic == "INT_LESSEQUAL" ||
	       mnemonic == "INT_SLESS" ||
	       mnemonic == "INT_LESS" ||
	       mnemonic == "BOOL_NEGATE" ||
	       mnemonic == "FLOAT_NEG" ||
	       mnemonic == "FLOAT_ABS" ||
	       mnemonic == "FLOAT_SQRT" ||
	       mnemonic == "FLOAT_CEIL" ||
	       mnemonic == "FLOAT_FLOOR" ||
	       mnemonic == "FLOAT_ROUND" ||
	       mnemonic == "FLOAT2FLOAT" ||
	       mnemonic == "FLOAT_NAN" ||
	       mnemonic == "FLOAT_EQUAL" ||
	       mnemonic == "FLOAT_LESSEQUAL" ||
	       mnemonic == "FLOAT_LESS" ||
	       mnemonic == "SUBPIECE" ||
	       mnemonic == "PIECE" ||
	       mnemonic == "POPCOUNT",
	    pcode_output(i, v_to),
	    pcode_input(i, _, v_from);

	o_assign(f.clone(), i.clone(), FlowType::Direct, v_to.clone(), EMPTY.clone(), v_from.clone(), EMPTY.clone()) <--
 	    stmt_in_function(i, f),
    	    pcode_mnemonic(i, mnemonic),
    	    if mnemonic == "INDIRECT",
    	    pcode_output(i, v_to),
     	    // Only uses arg 0 because INDIRECT arg 1 isn't a relevant data reference
    	    // in general
    	    pcode_input(i, 0, v_from);

	o_assign(f.clone(), i.clone(), FlowType::Direct, to.clone(), EMPTY.clone(), from.clone(), from_ap) <--
 	    stmt_in_function(i, f),
    	    field_access(i, from, from_ap, _),
    	    pcode_output(i, to);

// UNUSED:
/*            
	all_inputs_const(i.clone()) <--
	    inputs_const_up_to(i, n),
	    !pcode_input(i, n+1, _);

	inputs_const_up_to(i.clone(), 0) <--
	    pcode_input(i, 0, inp),
	    is_const_varnode(inp);

	inputs_const_up_to(i.clone(), n+1) <--
	    inputs_const_up_to(i, n),
	    pcode_input(i, n+1, inp),
	    is_const_varnode(inp);

	c_is_alloc(vn.clone(), EMPTY.clone(), func.clone()) <--
	    func_ptr(vn, func);

	c_is_alloc(v_from.clone(), EMPTY.clone(), hfunc_id.clone()),
*/
	o_assign(f.clone(), i.clone(), FlowType::Direct, v_to.clone(), EMPTY.clone(), v_from.clone(), EMPTY.clone()),
	assign_func_ptr_instruction(i.clone(), v_from.clone(), hfunc_id.clone()) <--
 	    stmt_in_function(i, f),
    	    pcode_mnemonic(i, Str::from("PTRSUB")),
    	    pcode_input(i, 0, zero),
    	    is_const_varnode(zero),
    	    vnode_offset_n(zero, 0),
    	    pcode_input(i, 1, v_from),
    	    func_ptr(v_from, hfunc_id),
    	    pcode_output(i, v_to);


	// ---------------------------------------------------------------------------
	// support

	// i64 the instructions from first (0) to end

	pcode_index_in_bb(bb.clone(), i, 0) <--
	    bb_first(bb, i);
	    
	pcode_index_in_bb(bb.clone(), next.clone(), n+1) <--
	    pcode_index_in_bb(bb, prev, n),
	    pcode_next(prev, next); 

	same_bb(bb, i0, i1) <--
	    pcode_index_in_bb(bb, i0, _), 
	    pcode_index_in_bb(bb, i1, _);
	    
	call_instruction(i.clone(), func_op.clone()) <--
	    (pcode_mnemonic(i, Str::from("CALL")) | pcode_mnemonic(i, Str::from("CALLIND"))),
	    pcode_input(i, 0, func_op);

	func_ptr(vn_func.clone(), func.clone()) <--
	    is_const_varnode(vn_func),
	    vnode_offset_n(vn_func, addr),
	    hfunc_ep(func, ep),
	    hfunc_local_ep(ep, addr);
  
// UNUSED:
/*   
	is_arith_mnemonic(m.clone()) <--
    	    pcode_mnemonic(_, m),
	    if m == "INT_ADD" ||
	      m == "INT_AND" ||
	      m == "INT_SRIGHT" ||
	      m == "FLOAT_ADD" ||
	      m == "FLOAT_DIV" ||
	      m == "FLOAT_MULT" ||
	      m == "FLOAT_SUB" ||
	      m == "INT_RIGHT" ||
	      m == "INT_MULT" ||
	      m == "INT_OR" ||
	      m == "INT_SDIV" ||
	      m == "INT_LEFT" ||
	      m == "INT_SREM" ||
	      m == "INT_SUB" ||
	      m == "INT_DIV" ||
	      m == "INT_REM" ||
	      m == "INT_XOR" ||
	      m == "INT_CARRY" ||
	      m == "INT_SCARRY" ||
	      m == "INT_SBORROW" ||
	      m == "BOOL_AND" ||
	      m == "BOOL_OR" ||
	      m == "BOOL_XOR" ||
	      m == "TRUNC" ||
	      m == "INT_SEXT" ||
	      m == "INT_ZEXT" ||
	      m == "INT_NEGATE" ||
	      m == "BOOL_NEGATE" ||
	      m == "FLOAT_NEG" ||
	      m == "FLOAT_ABS" ||
	      m == "FLOAT_SQRT" ||
	      m == "FLOAT_CEIL" ||
	      m == "FLOAT_FLOOR" ||
	      m == "FLOAT_ROUND" ||
	      m == "FLOAT2FLOAT" ||
	      m == "FLOAT_NAN";

	pointer_arith(vn.clone()) <--
	    pcode_output(i, vn),
	    pcode_mnemonic(i, mnemonic),
	    is_arith_mnemonic(mnemonic),
	    pcode_input(i, _, inp),
	    !is_const_varnode(inp);

	pointer_arith(vn.clone()) <--
	    pcode_output(i, vn),
	    pcode_mnemonic(i, mnemonic),
	    if mnemonic == "PTRADD",
	    (   (pcode_input(i, 1, index), !is_const_varnode(index)) |
		(pcode_input(i, 2, size), !is_const_varnode(size)));

	pointer_arith(out.clone()) <--
	    pointer_arith(inp),
	    pcode_input(i, _, inp),
	    maybe_cast_copy(out, inp),
	    pcode_output(i, out);
*/

	csourceinfo_location(i.clone(), 1.clone(), region_id.clone()),
	csourceinfo_address(region_id.clone()),
	caddress_absolute_address(region_id.clone(), target.clone()),
	caddress_fully_qualified_name(region_id.clone(), i.clone()),
	caddress_kind(region_id.clone(), Str::from("instruction")) <--
	    pcode_target(i, target),
   	    let region_id = i.as_ptr() as usize as i64;

	csourceinfo_location(vn.clone(), 1, region_id.clone()),
	csourceinfo_address(region_id.clone()),
	caddress_absolute_address(region_id.clone(), address.clone()),
	caddress_fully_qualified_name(region_id.clone(), vn.clone()),
	caddress_kind(region_id.clone(), Str::from("data")) <--
	    (cvar_in_function(vn, _) | cvar_is_global(vn)),
	    // it appears that the pc address can be missing if ghidra doesn't know the
	    // calling convention
	    vnode_pc_address(vn, address),
   	    let region_id = vn.as_ptr() as usize as i64;

	// same as above but gets name
	caddress_name(region_id.clone(), name.clone()) <--
	    (cvar_in_function(vn, _) | cvar_is_global(vn)),
	    vnode_pc_address(vn, address),
	    vnode_hvar(vn, hv),
	    hvar_name(hv, name),
   	    let region_id = vn.as_ptr() as usize as i64;

	csourceinfo_location(f.clone(), 1, region_id.clone()),
	csourceinfo_address(region_id.clone()),
	caddress_absolute_address(region_id.clone(), address.clone()),
	caddress_fully_qualified_name(region_id.clone(), f.clone()),
	caddress_kind(region_id.clone(), Str::from("function")) <--
	    hfunc_ep(f, ep),
	    (hfunc_local_ep(ep, address) | !hfunc_local_ep(ep, _), let address = ep),
   	    let region_id = f.as_ptr() as usize as i64;
   	    
	caddress_name(region_id.clone(), name.clone()) <--
	    hfunc_ep(f, ep),
	    hfunc_name(f, name),
  	    let region_id = f.as_ptr() as usize as i64;

// UNUSED:
/*   
	vnode(vn) <--
	    vnode_space(vn, sp),
	    if sp != "const";
*/	    

	// ---------------------------------------------------------------------------
	// typeprop

	relation is_type(Str);		
	relation is_inhabited_type(Str);		
	relation dereference_type(Str, Str);		
 	relation is_varnode(Str);
	relation c_is_field(Str);
	relation type_constraint_inout(Str, i64);
	relation type_constraint_inputs(Str);
	relation type_constraint_all(Str);
	relation type_constraint_edge(Str, Str);
	relation candidate_both(Str);
	relation candidate_inout(Str);
	relation candidate_inputs(Str);
	// a equivalent to b, ord(b) >= ord(a)
	relation type_constraint_ordered_edge(Str, Str);
	relation varnode_eq_class_rep(Str, Str);
	relation varnode_eq_class_reps(Str, Str);
	relation varnode_eq_class_reps_n(i64, Str);
	relation varnode_pointer_to_aggregate(Str, Str);

	// Varnode has constant value
	relation const_varnode(Str, i64);
	// Address of static data of given type
	relation static_struct_addr(i64, Str);
	// Varnode represents a static access of the field based off a struct at the address
	relation static_field_access(Str, i64, Str);

	is_varnode(vn) <--
	    vnode_space(vn, _);

	// inputs and output have same type
	candidate_inout(m) <--
    	   pcode_mnemonic(_, m),
	   if m == "INT_SRIGHT" ||
	      m == "INT_RIGHT" ||
	      m == "INT_LEFT" ||
	      m == "INDIRECT";

	// all inputs have equal types
	candidate_inputs(m) <--
    	   pcode_mnemonic(_, m),
	   if m == "INT_SCARRY" ||
	      m == "INT_SBORROW" ||
	      m == "INT_EQUAL" ||
	      m == "INT_SLESSEQUAL" ||
	      m == "INT_LESSEQUAL" ||
	      m == "INT_SLESS" ||
	      m == "INT_LESS" ||
	      m == "FLOAT_EQUAL" ||
	      m == "FLOAT_LESSEQUAL" ||
	      m == "FLOAT_LESS";

	candidate_both(m) <--
    	   pcode_mnemonic(_, m),
	   if m == "INT_ADD" ||
	      m == "INT_AND" ||
	      m == "INT_MULT" ||
	      m == "INT_OR" ||
	      m == "INT_SDIV" ||
	      m == "INT_SREM" ||
	      m == "INT_SUB" ||
	      m == "INT_DIV" ||
	      m == "INT_REM" ||
	      m == "INT_XOR" ||
	      m == "INT_CARRY" ||
	      m == "FLOAT_ADD" ||
	      m == "FLOAT_DIV" ||
	      m == "FLOAT_MULT" ||
	      m == "FLOAT_SUB" ||
	      m == "BOOL_AND" ||
	      m == "BOOL_OR" ||
	      m == "BOOL_XOR" ||
	      m == "COPY" ||
	      m == "MULTIEQUAL" ||
	      m == "INT_SEXT" ||
	      m == "INT_ZEXT" ||
	      m == "INT_2COMP" ||
	      m == "INT_NEGATE" ||
	      m == "BOOL_NEGATE" ||
	      m == "FLOAT_NEG" ||
	      m == "FLOAT_ABS" ||
	      m == "FLOAT_SQRT" ||
	      m == "FLOAT_CEIL" ||
	      m == "FLOAT_FLOOR" ||
	      m == "FLOAT_ROUND" ||
	      m == "FLOAT2FLOAT";
	      
	// an input and output have equal types
	type_constraint_edge(out, inp) <--
	    (candidate_both(mmnemonic) | candidate_inout(mnemonic)),
	    pcode_mnemonic(i, mnemonic),
	    pcode_output(i, out), 
	    pcode_input(i, n_in, inp);

	// all inputs have equal types
	type_constraint_edge(in1, in2) <--
	    (candidate_both(mmnemonic) | candidate_inputs(mnemonic)),
	    pcode_mnemonic(i, mnemonic), 
	    pcode_input(i, n1, in1), 
	    pcode_input(i, n2, in2), 
	    if n1 < n2;

	// ensure edges (l, r) have l less than or equal to r
	type_constraint_ordered_edge(dst, src) <-- 
	    type_constraint_edge(dst, src), 
	    let ndst = dst.as_ptr() as usize as i64,
	    let nsrc = src.as_ptr() as usize as i64,
	    if ndst > nsrc;
	type_constraint_ordered_edge(src, dst) <-- 
	    type_constraint_edge(dst, src), 
	    let ndst = dst.as_ptr() as usize as i64,
	    let nsrc = src.as_ptr() as usize as i64,
	    if ndst <= nsrc;

	// rep is ord of smallest element in the set
	varnode_eq_class_reps(a, a) <-- is_varnode(a);
	varnode_eq_class_reps(arep, b) <-- varnode_eq_class_reps(arep, a), type_constraint_ordered_edge(a, b);
	
	// choose smallest as rep
	//varnode_eq_class_reps(oldrep, a) <= varnode_eq_class_reps(newrep, a) <-- ord(newrep) < ord(oldrep);
	varnode_eq_class_reps_n(rv, v) <--
	    varnode_eq_class_reps(r, v),
	    let rv = r.as_ptr() as usize as i64;
	varnode_eq_class_rep(rep, v) <--
	    is_varnode(v),
    	    agg ref_value = min(r) in varnode_eq_class_reps_n(r, v),
    	    varnode_eq_class_reps(rep, v),
	    if ref_value == rep.as_ptr() as usize as i64;
	    
/*
	varnode_eq_class_rep(rep, v) <--
	    is_varnode(v),
	    let p = r.as_ptr() as usize as i64,
    	    agg ref_value = min(p) in varnode_eq_class_reps(r, v),
    	    varnode_eq_class_reps(rep, v),
	    if ref_value == rep.as_ptr() as usize as i64;
	    
	    let ref_value = varnode_eq_class_reps
		    .iter()
		    .filter(|(r, var)| var == &v)
		    .map(|(r, _)| r.as_ptr() as usize as i64)
		    .min()
		    .expect("No matching varnodes found"),
*/
	
	varnode_type_ghidra(vn, ttype) <--
	    hvar_class(hv, class),
	    // don't trust highother
	    if class != "other",
	    hvar_type(hv, ttype),
	    hvar_representative(hv, vn);

	varnode_type_ghidra(vn, ttype) <--
	    vnode_hvar(vn, hv),
	    hvar_type(hv, ttype);

	varnode_type_reaching(vn, t) <-- varnode_type_ghidra(vn, t);

	// propagate type to class rep
	varnode_type_reaching(rep, ttype) <--
	    varnode_type_reaching(vn, ttype),
	    varnode_eq_class_rep(rep, vn);

	// propagate class rep to members of class
	varnode_type_reaching(elt, ttype) <--
	    varnode_type_reaching(rep, ttype),
	    varnode_eq_class_rep(rep, elt);

	c_is_field(df) <-- 
	    type_struct_field_name(_, _, f),
	    let df = Str::from(format!(".{f}"));

	varnode_pointer_to_aggregate(vn, agg_ty) <--
	    varnode_type_reaching(vn, ty),
	    (type_pointer_base(ty, agg_ty) | type_array_base(ty, agg_ty)),
	    (type_struct(agg_ty)| type_union(agg_ty));

	// PTRSUB of structure
	field_access(i.clone(), structure_ptr.clone(), field_ap.clone(), field_type.clone()) <--
	    pcode_mnemonic(i, Str::from("PTRSUB")),

	    // base is structure
	    pcode_input(i, 0, structure_ptr),
	    varnode_type_reaching(structure_ptr, structure_ptr_type),
	    type_pointer(structure_ptr_type),
	    type_pointer_base(structure_ptr_type, structure_type),
	    type_struct(structure_type),

	    // offset field info
	    pcode_input(i, 1, field_offset_vn),
	    is_const_varnode(field_offset_vn),
	    vnode_offset_n(field_offset_vn, field_offset),
	    type_struct_offset_n(structure_type, field_index, field_offset),
	    type_struct_field(structure_type, field_index, field_type),
	    is_inhabited_type(field_type),
	    (  (type_struct_field_name(structure_type, field_index, field_name),
		let field_ap = Str::from(format!(".{field_name}")))
	    |  (!type_struct_field_name(structure_type, field_index, _),
		let field_ap = Str::from(format!(".[{field_offset}]"))));
		
	// PTRSUB of structure
	field_access(i.clone(), union_ptr.clone(), field_ap.clone(), field_type.clone()) <--
	    pcode_mnemonic(i, Str::from("PTRSUB")),

	    // base is structure
	    pcode_input(i, 0, union_ptr),
	    varnode_type_reaching(union_ptr, union_ptr_type),
	    type_pointer(union_ptr_type),
	    type_pointer_base(union_ptr_type, union_type),
	    type_union(union_type),

	    // offset field info
	    pcode_input(i, 1, field_offset_vn),
	    is_const_varnode(field_offset_vn),
	    vnode_offset_n(field_offset_vn, field_offset),
	    type_union_offset_n(union_type, field_index, field_offset),
	    type_union_field(union_type, field_index, field_type),
	    is_inhabited_type(field_type),
	    (  (type_union_field_name(union_type, field_index, field_name),
		let field_ap = Str::from(format!(".{field_name}")))
	    |  (!type_union_field_name(union_type, field_index, _),
		let field_ap = Str::from(format!(".[{field_offset}]"))));

	// PTRADD of array
	field_access(i.clone(), array_ptr.clone(), offset_ap.clone(), elt_type.clone()) <--
	    pcode_mnemonic(i, Str::from("PTRADD")),

	    // base is array
	    pcode_input(i, 0, array_ptr),
	    varnode_type_reaching(array_ptr, array_ptr_type),

	    // either points to an array or to a pointer
	    // get elt_type
	    dereference_type(array_ptr_type, elt_type),

	    // offset
	    pcode_input(i, 1, index_vn),
	    is_const_varnode(index_vn),
	    pcode_input(i, 2, size_vn),
	    is_const_varnode(size_vn),
	    vnode_offset_n(index_vn, index),
	    vnode_offset_n(size_vn, size),
	    let offset = index * size,
	    let offset_ap = Str::from(format!(".[{offset}]"));


	const_varnode(vn.clone(), value.clone()) <--
	    is_const_varnode(vn),
	    vnode_offset_n(vn, value);

	const_varnode(vn.clone(), value.clone()) <--
	    pcode_mnemonic(i, add),
	    (if add == "PTRSUB" || add == "INT_ADD"),
	    pcode_input(i, 0, in0),
	    pcode_input(i, 1, in1),

	    const_varnode(in0, value0),
	    const_varnode(in1, value1),

	    let value = value0 + value1,
	    pcode_output(i, vn);

	    
	static_struct_addr(addr.clone(), ty.clone()) <--
	    const_varnode(vn, addr),
	    vnode_hvar(vn, hv),
	    hvar_type(hv, ty),
	    type_pointer_base(ty, base_ty),
	    type_struct(base_ty);

	static_field_access(vn.clone(), start.clone(), Str::from(format!(".{name}"))) <--
	    static_struct_addr(start, ptr_ty),
	    type_pointer_base(ptr_ty, struct_ty),
	    type_struct_offset_n(struct_ty, n, field_offset),
	    let offset = start + field_offset,
	    vnode_offset_n(vn, offset),
	    vnode_space(vn, Str::from("ram")),
	    type_struct_field_name(struct_ty, n, name);
	    
	varnode_type_reaching(ptr.clone(), ptr_ty.clone()) <--
	    field_access(i, _, _, field_ty),
	    dereference_type(ptr_ty, field_ty),
	    pcode_output(i, ptr),
	    !varnode_type_ghidra(ptr, _);

	// propagate ptr base ty to load output
	varnode_type_reaching(out.clone(), base_ty.clone()) <--
	    varnode_type_reaching(ptr, ptr_ty),
	    pcode_input(i, 1, ptr),
	    pcode_mnemonic(i, Str::from("LOAD")),
	    pcode_output(i, out),
	    dereference_type(ptr_ty, base_ty),
	    !varnode_type_ghidra(out, _);

	// propagate load output type to ptr arg
	varnode_type_reaching(ptr.clone(), ptr_ty.clone()) <--
	    varnode_type_reaching(out, base_ty),
	    pcode_output(i, out),
	    pcode_mnemonic(i, Str::from("LOAD")),
	    dereference_type(ptr_ty, base_ty),
	    pcode_input(i, 1, ptr),
	    !varnode_type_ghidra(ptr, _);

	// propagate ptr type to stored val
	varnode_type_reaching(val.clone(), val_ty.clone()) <--
	    varnode_type_reaching(ptr, ptr_ty),
	    pcode_input(i, 1, ptr),
	    pcode_mnemonic(i, Str::from("STORE")),
	    dereference_type(ptr_ty, val_ty),
	    pcode_input(i, 2, val),
	    !varnode_type_ghidra(val, _);

	// propagate val type to stored ptr arg
	varnode_type_reaching(ptr.clone(), ptr_ty.clone()) <--
	    varnode_type_reaching(val, val_ty),
	    pcode_input(i, 2, val),
	    pcode_mnemonic(i, Str::from("STORE")),
	    dereference_type(ptr_ty, val_ty),
	    pcode_input(i, 1, ptr),
	    !varnode_type_ghidra(ptr, _);

	is_type(t.clone()) <--
	    type_length(t, _);

	is_inhabited_type(ttype.clone()) <--
	    type_length(ttype, len), 
	    if *len != 0; // void is 0, i hope

	dereference_type(ptr_ty.clone(), ty.clone()) <--
	    type_pointer(ptr_ty),
	    type_pointer_base(ptr_ty, ty);

	dereference_type(array_ty.clone(), ty.clone()) <--
	    type_array(array_ty),
	    type_array_base(array_ty, ty);

     };
    let elapsed = now.elapsed();
    log::info!("{:.2?} running rules", elapsed);

    Ok(ir::ConvertResult {
        formal_param: result.o_formal_param,
        actual_param: result.o_actual_param,
        call: result.o_call,
        call_site: result.o_call_site,
        assign: result.o_assign,
        paths: result.o_paths,
    })
}

fn make_field_ap(fld: &str, ty: &str) -> Str {
    (".".to_owned() + fld + ":<" + ty + ">").into()
}

/// If the index can be parsed as an i64, then the ap is at that index. If not, return None.
fn make_array_ap(idx: &Str) -> Option<Str> {
    match idx.parse::<i64>() {
        Ok(i) => Some(format!(".[{i}]").into()),
        Err(_) => None,
    }
}

fn make_global_field(cls: &str, fld: &str) -> Str {
    (".".to_owned() + fld + "@" + cls).into()
}
