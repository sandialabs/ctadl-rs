/*! Model support

Defines a [`ModelBuilders`] in which to express summary and call models.
*/

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::rc::Rc;
use std::sync::Arc;

use arrow::array::builder::{
    ArrayBuilder, BooleanBuilder, Int16Builder, StringBuilder, UInt8Builder, UInt64Builder,
};
use arrow::array::{
    ArrayRef, BooleanArray, Int16Array, RecordBatch, StringArray, UInt8Array, UInt64Array,
};
use arrow::compute::concat_batches;
use arrow::datatypes::{DataType, Field, Fields, Schema, SchemaBuilder, SchemaRef};
use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use itertools::izip;

use crate::error::{Error, ErrorContext};
use crate::facts::TaintDirection;
use ctadl_ir::ProgramInfo;

pub mod codegen;
pub mod json;
pub mod universe_set;

#[cfg(test)]
mod tests;

// TODO load models other than the default and load summary parquet models as well as json
pub fn try_load_default_models(program_info: &ProgramInfo) -> Result<ModelsBatch, Error> {
    log::trace!("load_models");
    // Load model_generator built-in models
    let default = include_bytes!("../languages/jadx/default-index.jsonl") as &[u8];
    let rdr = BufReader::new(default);
    try_load_jsonl_models(program_info, rdr).err_context(|| "loading default index models")
}

/// Load models from a `jsonl` source. `jsonl` allows streaming models one at a time efficiently.
/// The stream follows the same schema as elements of a `model_generators` array.
pub fn try_load_jsonl_models<B: BufRead>(
    program_info: &ProgramInfo,
    rdr: B,
) -> Result<ModelsBatch, Error> {
    let items = rdr.lines().map(|line| {
        let line = line?;
        serde_json::from_str(&line).err_context(|| "reading model line")
    });
    try_load_models_from_values(program_info, items)
}

// Load models from a JSON file containing `{ "model_generators": [...] }`.
///
/// The entries in the `model_generators` array are streamed into
/// `load_models_from_values`, preserving the existing batch‑processing logic.
pub fn try_load_json_models<P: AsRef<std::path::Path>>(
    program_info: &ProgramInfo,
    path: P,
) -> Result<ModelsBatch, Error> {
    // Open and parse the JSON file
    let file = File::open(&path)
        .err_context(|| format!("opening model JSON file: {}", path.as_ref().display()))?;
    let root: serde_json::Value = serde_json::from_reader(file)
        .err_context(|| format!("reading model JSON file: {}", path.as_ref().display()))?;

    // Extract the `model_generators` array; error if missing or not an array
    let generators = match root.get("model_generators").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid 'model_generators' array",
            )));
        }
    };

    // Stream each entry into the existing loader
    let items = generators.iter().cloned().map(Ok);
    try_load_models_from_values(program_info, items)
}

/// Load models from a JSON5 file containing `{ "model_generators": [...] }`.
pub fn try_load_json5_models<P: AsRef<std::path::Path>>(
    program_info: &ProgramInfo,
    path: P,
) -> Result<ModelsBatch, Error> {
    let mut file = File::open(&path)
        .err_context(|| format!("opening model JSON5 file: {}", path.as_ref().display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .err_context(|| format!("reading model JSON5 file: {}", path.as_ref().display()))?;
    let root: serde_json::Value = json5::from_str(&content)
        .err_context(|| format!("parsing model JSON5 file: {}", path.as_ref().display()))?;

    // Extract the `model_generators` array; error if missing or not an array
    let generators = match root.get("model_generators").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid 'model_generators' array",
            )));
        }
    };

    // Stream each entry into the existing loader
    let items = generators.iter().cloned().map(Ok);
    try_load_models_from_values(program_info, items)
}

/// Load models from a file. The file extension is used to decide whether to load as `json`,
/// `jsonl`, or `json5`.
pub fn try_load_models<P: AsRef<std::path::Path>>(
    program_info: &ProgramInfo,
    path: P,
) -> Result<ModelsBatch, Error> {
    let path = path.as_ref();
    let extension = path.extension().and_then(|s| s.to_str());
    match extension {
        Some("jsonl") => {
            let file = File::open(path)
                .err_context(|| format!("opening model JSONL file: {}", path.display()))?;
            let rdr = BufReader::new(file);
            try_load_jsonl_models(program_info, rdr)
        }
        Some("json5") => try_load_json5_models(program_info, path),
        _ => try_load_json_models(program_info, path),
    }
}

/// Load models from a stream of json Values. This processing is batched for efficiency, so the
/// iterator can be large and lazy.
pub fn try_load_models_from_values(
    program_info: &ProgramInfo,
    mut items: impl Iterator<Item = Result<serde_json::Value, Error>>,
) -> Result<ModelsBatch, Error> {
    let mut builder = ModelBuilders::new();
    let mut model_gen = json::ModelGeneratorIngest::new(program_info, &mut builder);
    let batch_size = 1024;
    let mut batch: Vec<serde_json::Value> = Vec::with_capacity(batch_size);

    loop {
        // Fill the batch
        for item in items.by_ref() {
            let item = item?;
            if batch.len() < batch_size {
                batch.push(item);
            } else {
                break;
            }
        }
        if batch.is_empty() {
            break;
        }
        // Process the batch
        model_gen
            .encode_models(batch.drain(..))
            .err_context(|| "encoding models".to_string())?;
        batch.clear();
    }
    log::trace!("matched {} summary models", builder.summary.len());
    log::trace!("matched {} source/sink models", builder.endpoint.len());
    let encmodels = builder.finish()?;
    Ok(encmodels)
}

/// A batch of encoded models
#[derive(Debug)]
pub struct ModelsBatch {
    pub summary: SummaryBatch,
    pub endpoint: EndpointBatch,
}

impl ModelsBatch {
    /// Concatenates two `EndpointBatch`s into one.
    /// Returns an error if any inner schemas differ.
    pub fn union_with(&mut self, other: &Self) -> Result<(), Error> {
        self.summary.union_with(&other.summary)?;
        self.endpoint.union_with(&other.endpoint)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct SummaryBatch {
    /// Table from [`SummaryBuilder`]
    pub summary: RecordBatch,
    pub aps: AccessPathBatch,
}

impl SummaryBatch {
    pub fn num_rows(&self) -> usize {
        self.summary.num_rows()
    }

    /// Concatenates two `SummaryBatch`s into one.
    /// Returns an error if any inner schemas differ.
    pub fn union_with(&mut self, other: &Self) -> Result<(), Error> {
        self.summary = concat_batches(&self.summary.schema(), [&self.summary, &other.summary])?;
        self.aps.union_with(&other.aps)?;
        Ok(())
    }

    /// Removes duplicate rows from this batch and returns a new deduplicated `SummaryBatch`.
    /// Duplicates are defined by the combination of function name, destination selector/index/path,
    /// source selector/index/path. The internal access‑path tables are rebuilt to stay consistent.
    pub fn dedup(&self) -> Result<Self, Error> {
        // Build map from AP id -> full ordered path components (Vec<String>)
        let ap_map = self.aps.build_ap_map();

        // Track keys we have already emitted
        let mut seen: HashSet<(
            String,
            u8,
            Option<i16>,
            Vec<String>,
            u8,
            Option<i16>,
            Vec<String>,
        )> = HashSet::new();

        // Builder for the deduped result
        let mut builder = SummaryBuilder::new();

        for (func, dst_tag, dst_index, dst_ap_id, src_tag, src_index, src_ap_id) in
            self.iter_summaries()
        {
            // Resolve paths for destination and source access‑paths
            let dst_path = ap_map.get(&dst_ap_id).cloned().unwrap_or_default();
            let src_path = ap_map.get(&src_ap_id).cloned().unwrap_or_default();

            let key = (
                func.to_string(),
                dst_tag as u8,
                dst_index,
                dst_path.clone(),
                src_tag as u8,
                src_index,
                src_path.clone(),
            );

            if !seen.contains(&key) {
                seen.insert(key);
                // Convert Vec<String> to slice of &str for the builder API
                let dst_slice: Vec<&str> = dst_path.iter().map(|s| s.as_str()).collect();
                let src_slice: Vec<&str> = src_path.iter().map(|s| s.as_str()).collect();

                builder.append(
                    func,
                    (dst_tag, dst_index, &dst_slice),
                    (src_tag, src_index, &src_slice),
                );
            }
        }

        builder.finish()
    }

    /// func, dst, src is the idea
    pub fn iter_summaries(
        &self,
    ) -> impl Iterator<
        Item = (
            &str,
            FormalIndexTypeTag,
            Option<i16>,
            u64,
            FormalIndexTypeTag,
            Option<i16>,
            u64,
        ),
    > {
        izip![
            self.summary
                .column_by_name("function")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .iter()
                .map(|s| s.unwrap()),
            self.summary
                .column_by_name("dst_selector_ty")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap().into()),
            self.summary
                .column_by_name("dst_index")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<Int16Array>()
                .unwrap()
                .iter(),
            self.summary
                .column_by_name("dst_ap")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
            self.summary
                .column_by_name("src_selector_ty")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap().into()),
            self.summary
                .column_by_name("src_index")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<Int16Array>()
                .unwrap()
                .iter(),
            self.summary
                .column_by_name("src_ap")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
        ]
    }
}

/// Main data type
#[derive(Debug)]
pub struct ModelBuilders {
    pub summary: SummaryBuilder,
    pub endpoint: EndpointBuilder,
}

impl Default for ModelBuilders {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelBuilders {
    pub fn new() -> Self {
        Self {
            summary: SummaryBuilder::new(),
            endpoint: EndpointBuilder::new(),
        }
    }

    pub fn finish(&mut self) -> Result<ModelsBatch, Error> {
        let summary = self.summary.finish()?;
        let endpoint = self.endpoint.finish()?;
        Ok(ModelsBatch { summary, endpoint })
    }
}

/// Builds function summaries efficiently. A summary is a function together with source and
/// destination parameter and access path information.
#[derive(Debug)]
pub struct SummaryBuilder {
    func: StringBuilder,
    dst_index: FormalIndexBuilder,
    dst_path_id: UInt64Builder,
    src_index: FormalIndexBuilder,
    src_path_id: UInt64Builder,
    ap_len: AccessPathBuilder,
    ap_fld: Rc<RefCell<AccessPathFieldBuilder>>,
}

impl Default for SummaryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SummaryBuilder {
    pub fn new() -> Self {
        let ap_fld = Rc::new(RefCell::new(AccessPathFieldBuilder::new()));
        Self {
            func: Default::default(),
            dst_index: FormalIndexBuilder::new("dst_"),
            dst_path_id: UInt64Builder::new(),
            src_index: FormalIndexBuilder::new("src_"),
            src_path_id: UInt64Builder::new(),
            ap_len: AccessPathBuilder::new("", ap_fld.clone()),
            ap_fld,
        }
    }

    #[inline]
    pub fn append(
        &mut self,
        function: &str,
        dst: (FormalIndexTypeTag, Option<i16>, &[&str]),
        src: (FormalIndexTypeTag, Option<i16>, &[&str]),
    ) {
        let (dst_ty, dst_index, dst_path) = dst;
        let (src_ty, src_index, src_path) = src;
        self.func.append_value(function);
        self.dst_index.append(dst_ty, dst_index);
        let dst_path_id = self.ap_len.append(dst_path);
        self.dst_path_id.append_value(dst_path_id);
        self.src_index.append(src_ty, src_index);
        let src_path_id = self.ap_len.append(src_path);
        self.src_path_id.append_value(src_path_id);
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.func.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.func.is_empty()
    }

    /// Columns in the schema:
    /// - function: name of function
    /// - dst_selector_ty
    /// - dst_index
    /// - dst_ap: id of ap into the ap_len table
    /// - src_selector_ty
    /// - src_index
    /// - src_ap
    #[inline]
    pub fn finish(&mut self) -> Result<SummaryBatch, Error> {
        let func = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "func",
                DataType::Utf8,
                false,
            )]))),
            vec![Arc::new(self.func.finish())],
        )?;
        let dst_index = self.dst_index.finish()?;
        let dst_ap = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "dst_ap",
                DataType::UInt64,
                false,
            )]))),
            vec![Arc::new(self.dst_path_id.finish())],
        )?;
        let src_index = self.src_index.finish()?;
        let src_ap = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "src_ap",
                DataType::UInt64,
                false,
            )]))),
            vec![Arc::new(self.src_path_id.finish())],
        )?;
        let ap_len = self.ap_len.finish()?;
        let ap_fld = self.ap_fld.borrow_mut().finish()?;
        let summary_schema: SchemaRef = {
            let mut b = SchemaBuilder::new();
            b.push(Field::new("function", DataType::Utf8, false));
            b.extend(dst_index.schema_ref().fields().to_vec());
            b.push(Field::new("dst_ap", DataType::UInt64, false));
            b.extend(src_index.schema_ref().fields().to_vec());
            b.push(Field::new("src_ap", DataType::UInt64, false));
            b.finish().into()
        };
        let mut data = Vec::new();
        data.extend(func.columns().iter().cloned());
        data.extend(dst_index.columns().iter().cloned());
        data.extend(dst_ap.columns().iter().cloned());
        data.extend(src_index.columns().iter().cloned());
        data.extend(src_ap.columns().iter().cloned());
        let summary = RecordBatch::try_new(summary_schema.clone(), data)?;

        Ok(SummaryBatch {
            summary,
            aps: AccessPathBatch { ap_len, ap_fld },
        })
    }
}

#[derive(Debug)]
pub struct FormalIndexBuilder {
    pub schema: SchemaRef,
    selector_ty: UInt8Builder,
    index: Int16Builder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormalIndexTypeTag {
    /// Index of a formal for the builder
    Index,
    /// Return value
    Return,
    /// Global value
    Global,
    /// Any parameter (excluding return and global)
    AnyArgument,
}

impl From<FormalIndexTypeTag> for u8 {
    #[inline]
    fn from(t: FormalIndexTypeTag) -> u8 {
        use FormalIndexTypeTag::*;
        match t {
            Index => 0,
            Return => 1,
            Global => 2,
            AnyArgument => 3,
        }
    }
}

impl From<u8> for FormalIndexTypeTag {
    #[inline]
    fn from(t: u8) -> FormalIndexTypeTag {
        use FormalIndexTypeTag::*;
        match t {
            0 => Index,
            1 => Return,
            2 => Global,
            3 => AnyArgument,
            _ => panic!("bad FormalIndexTypeTag"),
        }
    }
}

impl FormalIndexBuilder {
    pub fn new(prefix: &str) -> Self {
        let mut b = SchemaBuilder::new();
        b.push(Field::new(
            format!("{prefix}selector_ty"),
            DataType::UInt8,
            false,
        ));
        b.push(Field::new(format!("{prefix}index"), DataType::Int16, true));
        let schema = b.finish().into();
        Self {
            schema,
            selector_ty: Default::default(),
            index: Default::default(),
        }
    }

    #[inline]
    pub fn append(&mut self, ty: FormalIndexTypeTag, index: Option<i16>) {
        self.selector_ty.append_value(ty.into());
        self.index.append_option(index);
    }

    #[inline]
    pub fn finish(&mut self) -> Result<RecordBatch, Error> {
        let v: Vec<ArrayRef> = vec![
            Arc::new(self.selector_ty.finish()),
            Arc::new(self.index.finish()),
        ];
        let b = RecordBatch::try_new(self.schema.clone(), v)?;
        Ok(b)
    }
}

/// Builds a table of "id" and "len", one entry per access path
#[derive(Debug)]
pub struct AccessPathBuilder {
    pub schema: SchemaRef,
    id: UInt64Builder,
    len: UInt8Builder,
    fields: Rc<RefCell<AccessPathFieldBuilder>>,
}

impl AccessPathBuilder {
    /// The prefix is concatenated to the front of the field names
    pub fn new(prefix: &str, fields: Rc<RefCell<AccessPathFieldBuilder>>) -> Self {
        let mut b = SchemaBuilder::new();
        b.push(Field::new(format!("{prefix}id"), DataType::UInt64, false));
        b.push(Field::new(format!("{prefix}len"), DataType::UInt8, false));
        let schema = b.finish().into();
        Self {
            schema,
            id: Default::default(),
            len: Default::default(),
            fields,
        }
    }

    #[inline]
    pub fn append(&mut self, ap: &[&str]) -> u64 {
        let id = self.id.len().try_into().expect("too many APs");
        self.id.append_value(id);
        self.len
            .append_value(ap.len().try_into().expect("AP too big"));
        for (pos, field) in ap.iter().enumerate() {
            self.fields
                .borrow_mut()
                .append(id, pos.try_into().expect("too many fields"), field);
        }
        id
    }

    #[inline]
    pub fn finish(&mut self) -> Result<RecordBatch, Error> {
        let v: Vec<ArrayRef> = vec![Arc::new(self.id.finish()), Arc::new(self.len.finish())];
        let ap = RecordBatch::try_new(self.schema.clone(), v)?;
        Ok(ap)
    }
}

/// Builds a table of "id", "pos", and "field"
#[derive(Debug)]
pub struct AccessPathFieldBuilder {
    pub schema: SchemaRef,
    id: UInt64Builder,
    pos: UInt8Builder,
    field: StringBuilder,
}

impl Default for AccessPathFieldBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessPathFieldBuilder {
    pub fn new() -> Self {
        let mut b = SchemaBuilder::new();
        b.push(Field::new("id", DataType::UInt64, false));
        b.push(Field::new("pos", DataType::UInt8, false));
        b.push(Field::new("field", DataType::Utf8, false));
        let schema = b.finish().into();
        Self {
            schema,
            id: Default::default(),
            pos: Default::default(),
            field: Default::default(),
        }
    }

    #[inline]
    pub fn append(&mut self, ap_id: u64, pos: u8, field: &str) {
        self.id.append_value(ap_id);
        self.pos.append_value(pos);
        self.field.append_value(field);
    }

    #[inline]
    pub fn finish(&mut self) -> Result<RecordBatch, Error> {
        let v: Vec<ArrayRef> = vec![
            Arc::new(self.id.finish()),
            Arc::new(self.pos.finish()),
            Arc::new(self.field.finish()),
        ];
        let b = RecordBatch::try_new(self.schema.clone(), v)?;
        Ok(b)
    }
}

#[derive(Debug)]
pub struct EndpointBuilder {
    /// Name of the endpoint function called
    func: StringBuilder,
    /// Argument of the endpoint
    index: FormalIndexBuilder,
    /// ID of the access path
    path_id: UInt64Builder,
    /// Taint label
    label: StringBuilder,
    /// Use `true` for the forward direction and `false` for backward
    direction: BooleanBuilder,
    /// Access path length table: `id` and `len`
    ap_len: AccessPathBuilder,
    /// Access path field table: `id`
    ap_fld: Rc<RefCell<AccessPathFieldBuilder>>,
}

impl Default for EndpointBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl EndpointBuilder {
    /// Create a new empty endpoint builder.
    pub fn new() -> Self {
        let ap_fld = Rc::new(RefCell::new(AccessPathFieldBuilder::new()));
        Self {
            func: Default::default(),
            // No prefix – column names will be "selector_ty" and "index"
            index: FormalIndexBuilder::new(""),
            path_id: UInt64Builder::new(),
            label: Default::default(),
            direction: BooleanBuilder::new(),
            ap_len: AccessPathBuilder::new("", ap_fld.clone()),
            ap_fld,
        }
    }

    /// Append an endpoint entry.
    /// `function` – name of the function containing the endpoint.
    /// `idx` – selector type tag and optional formal index for the variable.
    /// `ap` – access‑path components (as string slices).
    /// `label` – label associated with the endpoint.
    /// `direction` – true for forward (source), false for backward (sink).
    pub fn append(
        &mut self,
        function: &str,
        idx: (FormalIndexTypeTag, Option<i16>),
        ap: &[&str],
        label: &str,
        direction: TaintDirection,
    ) {
        let (tag, opt_idx) = idx;
        self.func.append_value(function);
        self.index.append(tag, opt_idx);
        let path_id_val = self.ap_len.append(ap);
        self.path_id.append_value(path_id_val);
        self.label.append_value(label);
        self.direction
            .append_value(direction == TaintDirection::Forward);
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.func.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.func.is_empty()
    }

    /// Build the `EndpointBatch` containing endpoint records and access‑path tables.
    /// Columns:
    /// - function
    /// - selector_ty
    /// - index
    /// - path_id
    /// - label
    /// - direction
    pub fn finish(&mut self) -> Result<EndpointBatch, Error> {
        // function column
        let func = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "function",
                DataType::Utf8,
                false,
            )]))),
            vec![Arc::new(self.func.finish())],
        )?;
        // index columns (selector_ty + index)
        let idx_batch = self.index.finish()?;
        // path_id column
        let path_id = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "path_id",
                DataType::UInt64,
                false,
            )]))),
            vec![Arc::new(self.path_id.finish())],
        )?;
        // label column
        let lbl = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "label",
                DataType::Utf8,
                false,
            )]))),
            vec![Arc::new(self.label.finish())],
        )?;
        // direction column (boolean)
        let dir = RecordBatch::try_new(
            Arc::new(Schema::new(Fields::from(vec![Field::new(
                "direction",
                DataType::Boolean,
                false,
            )]))),
            vec![Arc::new(self.direction.finish())],
        )?;

        // Build final schema: function, index fields, path_id, label, direction
        let endpoint_schema: SchemaRef = {
            let mut b = SchemaBuilder::new();
            b.push(Field::new("function", DataType::Utf8, false));
            b.extend(idx_batch.schema_ref().fields().to_vec());
            b.push(Field::new("path_id", DataType::UInt64, false));
            b.push(Field::new("label", DataType::Utf8, false));
            b.push(Field::new("direction", DataType::Boolean, false));
            b.finish().into()
        };
        // Assemble columns in the same order as the schema
        let mut data = Vec::new();
        data.extend(func.columns().iter().cloned());
        data.extend(idx_batch.columns().iter().cloned());
        data.extend(path_id.columns().iter().cloned());
        data.extend(lbl.columns().iter().cloned());
        data.extend(dir.columns().iter().cloned());

        let records = RecordBatch::try_new(endpoint_schema.clone(), data)?;
        // Access‑path auxiliary tables
        let ap_len = self.ap_len.finish()?;
        let ap_fld = self.ap_fld.borrow_mut().finish()?;
        Ok(EndpointBatch {
            endpoints: records,
            aps: AccessPathBatch { ap_len, ap_fld },
        })
    }
}

#[derive(Debug)]
pub struct EndpointBatch {
    pub endpoints: RecordBatch,
    pub aps: AccessPathBatch,
}

impl EndpointBatch {
    /// Concatenates two `EndpointBatch`s into one.
    /// Returns an error if any inner schemas differ.
    pub fn union_with(&mut self, other: &Self) -> Result<(), Error> {
        self.endpoints = concat_batches(
            &self.endpoints.schema(),
            [&self.endpoints, &other.endpoints],
        )?;
        self.aps.union_with(&other.aps)?;
        Ok(())
    }

    /// Iterate over endpoint records.
    /// Yields `(function, selector_ty, index, path_id, label, direction)`.
    pub fn iter_endpoints(
        &self,
    ) -> impl Iterator<
        Item = (
            &str,
            FormalIndexTypeTag,
            Option<i16>,
            u64,
            &str,
            TaintDirection,
        ),
    > {
        izip![
            self.endpoints
                .column_by_name("function")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .iter()
                .map(|s| s.unwrap()),
            self.endpoints
                .column_by_name("selector_ty")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap().into()),
            self.endpoints
                .column_by_name("index")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<Int16Array>()
                .unwrap()
                .iter(),
            self.endpoints
                .column_by_name("path_id")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
            self.endpoints
                .column_by_name("label")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .iter()
                .map(|s| s.unwrap()),
            self.endpoints
                .column_by_name("direction")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .iter()
                .map(|b| {
                    let v = b.unwrap();
                    if v {
                        TaintDirection::Forward
                    } else {
                        TaintDirection::Backward
                    }
                }),
        ]
    }
}

#[derive(Debug)]
pub struct AccessPathBatch {
    /// Table from [`AccessPathBuilder`]
    pub ap_len: RecordBatch,
    /// Table from [`AccessPathFieldBuilder`]
    pub ap_fld: RecordBatch,
}

impl AccessPathBatch {
    pub fn union_with(&mut self, other: &Self) -> Result<(), Error> {
        self.ap_len = concat_batches(&self.ap_len.schema(), [&self.ap_len, &other.ap_len])?;
        self.ap_fld = concat_batches(&self.ap_fld.schema(), [&self.ap_fld, &other.ap_fld])?;
        Ok(())
    }

    /// Helper that creates a map from AP id -> ordered list of path components.
    pub fn build_ap_map(&self) -> HashMap<u64, Vec<String>> {
        // Collect lengths per AP
        let mut len_map: HashMap<u64, u8> = self.iter_ap_len().collect();
        // Temporary storage for fields by (ap_id, pos)
        let mut field_by_pos: HashMap<u64, HashMap<u8, String>> = HashMap::new();
        for (id, pos, field) in self.iter_ap_fld() {
            field_by_pos
                .entry(id)
                .or_default()
                .insert(pos, field.to_string());
        }
        // Assemble final vectors respecting order 0..len-1
        let mut result: HashMap<u64, Vec<String>> = HashMap::new();
        for (ap_id, len) in len_map.drain() {
            if let Some(pos_map) = field_by_pos.get(&ap_id) {
                let mut vec = Vec::with_capacity(len as usize);
                for p in 0..len {
                    // Unwrap is safe because the builder always filled each position
                    let f = pos_map.get(&{ p }).expect("missing AP field");
                    vec.push(f.clone());
                }
                result.insert(ap_id, vec);
            } else {
                result.insert(ap_id, Vec::new());
            }
        }
        result
    }

    pub fn iter_ap_len(&self) -> impl Iterator<Item = (u64, u8)> {
        izip![
            self.ap_len
                .column_by_name("id")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
            self.ap_len
                .column_by_name("len")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
        ]
    }

    pub fn iter_ap_fld(&self) -> impl Iterator<Item = (u64, u8, &str)> {
        izip![
            self.ap_fld
                .column_by_name("id")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
            self.ap_fld
                .column_by_name("pos")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<UInt8Array>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
            self.ap_fld
                .column_by_name("field")
                .unwrap()
                .as_ref()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .iter()
                .map(|u| u.unwrap()),
        ]
    }
}
