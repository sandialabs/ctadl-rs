//! Read/write facts as parquet files.
#![allow(unused_parens)]

use std::{fs::File, sync::Arc};

use arrow::{
    array::{
        Array, ArrayRef, BooleanArray, GenericStringArray, Int8Array, Int16Array, Int32Array,
        Int64Array, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
    },
    datatypes as arrowd,
    record_batch::RecordBatch,
};
use internment::ArcIntern;
use itertools::{Itertools, izip, multiunzip};
use parquet::arrow::*;
use parquet::{
    arrow::arrow_reader::{
        ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder, statistics::StatisticsConverter,
    },
    basic::Compression,
    file::properties::WriterProperties,
};
use paste::paste;

use crate::error::{Error, ErrorContext};
use crate::facts;

type Str = facts::Str;

pub trait Encoder<T> {
    type Output;

    fn encode(&mut self, value: T);
    fn finish(self) -> Self::Output;
}

pub trait Decoder<T> {
    type Input;
    fn new(input: Self::Input) -> Self;
    fn decode(&mut self) -> Option<T>;
}

mod encoders {
    #![allow(unused, dead_code)]
    use super::*;

    pub struct Nullable<E> {
        inner: E,
        def_levels: Vec<u8>,
    }

    impl<T, E> Encoder<Option<T>> for Nullable<E>
    where
        E: Encoder<T>,
    {
        type Output = (Vec<u8>, E::Output);

        fn encode(&mut self, v: Option<T>) {
            match v {
                Some(x) => {
                    self.def_levels.push(1);
                    self.inner.encode(x);
                }
                None => {
                    self.def_levels.push(0);
                }
            }
        }

        fn finish(self) -> Self::Output {
            (self.def_levels, self.inner.finish())
        }
    }
}

mod decoders {
    #![allow(unused, dead_code)]
    use super::*;

    pub struct Nullable<D> {
        inner: D,
        def_levels: Vec<u8>,
        pos: usize,
    }

    impl<T, D> Decoder<Option<T>> for Nullable<D>
    where
        D: Decoder<T>,
    {
        type Input = (Vec<u8>, D::Input);

        fn new((def_levels, input): Self::Input) -> Self {
            Self {
                inner: D::new(input),
                def_levels,
                pos: 0,
            }
        }

        fn decode(&mut self) -> Option<Option<T>> {
            if self.pos >= self.def_levels.len() {
                return None;
            }

            let def = self.def_levels[self.pos];
            self.pos += 1;
            if def == 0 {
                Some(None)
            } else {
                Some(Some(self.inner.decode()?))
            }
        }
    }
}

// dyn arrow::array::Array is what all the columnar data goes to

pub trait EncodeColumns<T> {
    /// Encodes a vec of tuples into fields and arrays
    fn encode_all(names: &[&str], data: Vec<T>) -> (Vec<arrowd::Field>, Vec<ArrayRef>);
}

pub trait DecodeColumns<T> {
    /// Decodes tuples from the given batch
    fn decode_tuples(names: &[&str], batch: &RecordBatch) -> Vec<T>;
}

pub trait EncodeColumn<T> {
    /// Encodes a column vec into a corresponding fields and arrays. The method can output multiple
    /// fields. This way a complex type can be encoded into multiple columns.
    fn encode_column(name: &str, col: Vec<T>) -> (Vec<arrowd::Field>, Vec<ArrayRef>);
}

pub trait DecodeColumn<T> {
    /// Decodes the records from the given batch
    fn into_decode_array(name: &str, batch: &RecordBatch) -> impl IntoIterator<Item = T>;
}

// ---------------------------------------------------------------------------
// Impl

pub struct Reader {
    path: std::path::PathBuf,
}

/// Selects the rows whose `column`, a [`facts::FunctionId`] column, holds one of `ids`.
///
/// A reader given one skips each row group whose `column` statistics show it holds none of
/// `ids`, then drops the other rows of the groups it decodes. A file sorted by `column` keeps
/// each function's rows in few groups, so a reader decodes little beyond them.
#[derive(Clone, Copy, Debug)]
pub struct FunctionFilter<'a> {
    column: &'a str,
    ids: &'a [facts::FunctionId],
}

impl<'a> FunctionFilter<'a> {
    /// `ids` must be sorted and distinct.
    pub fn new(column: &'a str, ids: &'a [facts::FunctionId]) -> Self {
        debug_assert!(
            ids.is_sorted_by(|a, b| a < b),
            "ids are sorted and distinct"
        );
        Self { column, ids }
    }

    /// Whether some id lies in `min..=max`.
    fn meets(&self, min: u32, max: u32) -> bool {
        let at = self.ids.partition_point(|f| f.id < min);
        at < self.ids.len() && self.ids[at].id <= max
    }

    /// `batch` without the rows whose id is not selected.
    fn apply(&self, batch: RecordBatch) -> Result<RecordBatch, Error> {
        let ids = batch
            .column_by_name(self.column)
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
            .ok_or_else(|| {
                Error::FactsConvert(format!("no function id column {:?}", self.column))
            })?;
        let keep: BooleanArray = ids
            .values()
            .iter()
            .map(|id| Some(self.ids.binary_search_by_key(id, |f| f.id).is_ok()))
            .collect();
        if keep.true_count() == batch.num_rows() {
            return Ok(batch);
        }
        Ok(arrow::compute::filter_record_batch(&batch, &keep)?)
    }
}

/// The rows of a file, decoded one record batch at a time; see [`Reader::rows`].
pub struct Rows<'a, T> {
    reader: ParquetRecordBatchReader,
    columns: &'a [&'a str],
    filter: Option<FunctionFilter<'a>>,
    decoded: std::vec::IntoIter<T>,
    path: std::path::PathBuf,
}

impl<T> Iterator for Rows<'_, T>
where
    DefaultDecoder: DecodeColumns<T>,
{
    type Item = Result<T, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.decoded.next() {
                return Some(Ok(row));
            }
            let batch = match self.reader.next()? {
                Ok(batch) => batch,
                Err(e) => {
                    let path = self.path.display().to_string();
                    return Some(Err(e).err_context(|| format!("reading parquet file: {path}")));
                }
            };
            let batch = match self.filter {
                Some(filter) => match filter.apply(batch) {
                    Ok(batch) => batch,
                    Err(e) => return Some(Err(e)),
                },
                None => batch,
            };
            self.decoded = DefaultDecoder::decode_tuples(self.columns, &batch).into_iter();
        }
    }
}

impl Reader {
    /// Reads from the given path
    pub fn new<P: AsRef<std::path::Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }

    fn context(&self) -> String {
        format!("reading parquet file: {}", self.path.display())
    }

    fn builder(&self) -> Result<ParquetRecordBatchReaderBuilder<File>, Error> {
        let file = File::open(&self.path)
            .map_err(Error::from)
            .err_context(|| format!("opening parquet file: {}", self.path.display()))?;
        ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(Error::Parquet)
            .err_context(|| self.context())
    }

    /// The number of row groups in the file.
    pub fn num_row_groups(&self) -> Result<usize, Error> {
        Ok(self.builder()?.metadata().num_row_groups())
    }

    /// The row groups a reader given `filter` decodes: those whose statistics do not rule out
    /// every selected id. A group without statistics is kept.
    pub fn row_groups_for(&self, filter: FunctionFilter<'_>) -> Result<Vec<usize>, Error> {
        Self::select_row_groups(&self.builder()?, filter).err_context(|| self.context())
    }

    fn select_row_groups(
        builder: &ParquetRecordBatchReaderBuilder<File>,
        filter: FunctionFilter<'_>,
    ) -> Result<Vec<usize>, Error> {
        let groups = builder.metadata().row_groups();
        let stats = StatisticsConverter::try_new(
            filter.column,
            builder.schema(),
            builder.parquet_schema(),
        )?;
        let mins = stats.row_group_mins(groups.iter())?;
        let maxes = stats.row_group_maxes(groups.iter())?;
        let (Some(mins), Some(maxes)) = (
            mins.as_any().downcast_ref::<UInt32Array>(),
            maxes.as_any().downcast_ref::<UInt32Array>(),
        ) else {
            return Err(Error::FactsConvert(format!(
                "column {:?} is not a function id column",
                filter.column
            )));
        };
        Ok((0..groups.len())
            .filter(|&i| {
                mins.is_null(i) || maxes.is_null(i) || filter.meets(mins.value(i), maxes.value(i))
            })
            .collect())
    }

    /// Streams the file's rows, using the given column names, one record batch at a time. A
    /// batch holds at most one row group's rows, so memory holds one decoded group, not the
    /// file. The iterator can feed [`Writer::write_rows`].
    pub fn rows<'a, T>(&self, column_names: &'a [&'a str]) -> Result<Rows<'a, T>, Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        self.open(column_names, None)
    }

    /// As [`Reader::rows`], only the rows `filter` selects.
    pub fn rows_of<'a, T>(
        &self,
        column_names: &'a [&'a str],
        filter: FunctionFilter<'a>,
    ) -> Result<Rows<'a, T>, Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        self.open(column_names, Some(filter))
    }

    fn open<'a, T>(
        &self,
        columns: &'a [&'a str],
        filter: Option<FunctionFilter<'a>>,
    ) -> Result<Rows<'a, T>, Error> {
        let mut builder = self.builder()?;
        let largest_group = builder
            .metadata()
            .row_groups()
            .iter()
            .map(|g| g.num_rows() as usize)
            .max()
            .unwrap_or(0);
        builder = builder.with_batch_size(largest_group.max(1));
        if let Some(filter) = filter {
            let groups =
                Self::select_row_groups(&builder, filter).err_context(|| self.context())?;
            builder = builder.with_row_groups(groups);
        }
        let reader = builder
            .build()
            .map_err(Error::Parquet)
            .err_context(|| self.context())?;
        Ok(Rows {
            reader,
            columns,
            filter,
            decoded: Vec::new().into_iter(),
            path: self.path.clone(),
        })
    }

    /// Calls `f` on the file's rows, one record batch at a time, as [`Reader::rows`] decodes
    /// them.
    pub fn for_each_batch<T>(
        &self,
        column_names: &[&str],
        f: impl FnMut(Vec<T>) -> Result<(), Error>,
    ) -> Result<(), Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        self.each_batch(column_names, None, f)
    }

    /// As [`Reader::for_each_batch`], only the rows `filter` selects. A batch may be empty.
    pub fn for_each_batch_of<T>(
        &self,
        column_names: &[&str],
        filter: FunctionFilter<'_>,
        f: impl FnMut(Vec<T>) -> Result<(), Error>,
    ) -> Result<(), Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        self.each_batch(column_names, Some(filter), f)
    }

    fn each_batch<T>(
        &self,
        column_names: &[&str],
        filter: Option<FunctionFilter<'_>>,
        mut f: impl FnMut(Vec<T>) -> Result<(), Error>,
    ) -> Result<(), Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        let rows = self.open::<T>(column_names, filter)?;
        for batch in rows.reader {
            let batch = batch.map_err(Error::Arrow).err_context(|| self.context())?;
            let batch = match filter {
                Some(filter) => filter.apply(batch)?,
                None => batch,
            };
            f(DefaultDecoder::decode_tuples(column_names, &batch))?;
        }
        Ok(())
    }

    /// Read a parquet file into a vec. The vec rows are read using the given column names
    pub fn read_vec<T>(&self, column_names: &[&str]) -> Result<Vec<T>, Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        let mut result = Vec::new();
        self.for_each_batch(column_names, |rows| {
            result.extend(rows);
            Ok(())
        })?;
        result.shrink_to_fit();
        Ok(result)
    }
}

/// Rows per row group of a written file.
pub const ROWS_PER_GROUP: usize = 64 * 1024;

pub struct Writer {
    path: std::path::PathBuf,
    rows_per_group: usize,
}

impl Writer {
    /// Writes to the given path, [`ROWS_PER_GROUP`] rows per row group.
    pub fn new<P: AsRef<std::path::Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().into(),
            rows_per_group: ROWS_PER_GROUP,
        }
    }

    /// Sets the most rows a row group holds. Smaller groups let a [`FunctionFilter`] skip more
    /// of a file, at some cost in size.
    pub fn rows_per_group(mut self, rows: usize) -> Self {
        assert!(rows > 0, "a row group holds at least one row");
        self.rows_per_group = rows;
        self
    }

    /// Writes the rows to a parquet file, using the given column names, up to
    /// [`Writer::rows_per_group`] rows at a time: each batch is encoded, written out as a row
    /// group of its own and dropped before the next is taken, so memory holds one batch's
    /// columns, not the whole table's.
    pub fn write_iter<T>(
        &mut self,
        column_names: &[&str],
        rows: impl IntoIterator<Item = T>,
    ) -> Result<(), Error>
    where
        DefaultEncoder: EncodeColumns<T>,
    {
        self.write_rows(column_names, rows.into_iter().map(Ok))
    }

    /// As [`Writer::write_iter`], from rows that may fail to come, such as those of
    /// [`Reader::rows`]. The first error stops the write and is returned; the file is then
    /// incomplete.
    pub fn write_rows<T>(
        &mut self,
        column_names: &[&str],
        rows: impl IntoIterator<Item = Result<T, Error>>,
    ) -> Result<(), Error>
    where
        DefaultEncoder: EncodeColumns<T>,
    {
        let per_group = self.rows_per_group;
        let mut rows = rows.into_iter();
        let groups = std::iter::from_fn(move || {
            let group: Result<Vec<T>, Error> = rows.by_ref().take(per_group).collect();
            match group {
                Ok(group) if group.is_empty() => None,
                group => Some(group),
            }
        });
        self.write_groups(column_names, groups)
    }

    /// As [`Writer::write_iter`], and a row group also ends wherever `key` changes from one
    /// row to the next. Rows sorted by a key that partitions them, such as their batch, then
    /// never share a row group with another part, unless there are so many parts that a group
    /// each would pass [`MAX_KEYED_ROW_GROUPS`]: then a group ends at a key change only once it
    /// holds enough rows to stay under it.
    pub fn write_iter_grouped_by<T, K: PartialEq>(
        &mut self,
        column_names: &[&str],
        rows: impl IntoIterator<Item = T, IntoIter: ExactSizeIterator>,
        mut key: impl FnMut(&T) -> K,
    ) -> Result<(), Error>
    where
        DefaultEncoder: EncodeColumns<T>,
    {
        let per_group = self.rows_per_group;
        let rows = rows.into_iter();
        let fewest = rows.len().div_ceil(MAX_KEYED_ROW_GROUPS).min(per_group);
        let mut rows = rows.peekable();
        let groups = std::iter::from_fn(move || {
            let first = rows.next()?;
            let mut k = key(&first);
            let mut group = vec![first];
            while group.len() < per_group {
                let Some(next) = rows.peek() else { break };
                let next_key = key(next);
                if next_key != k {
                    if group.len() >= fewest {
                        break;
                    }
                    k = next_key;
                }
                group.push(rows.next().expect("peeked"));
            }
            Some(Ok(group))
        });
        self.write_groups(column_names, groups)
    }

    /// Writes each group as a row group of its own. A file with no rows still gets the schema.
    fn write_groups<T>(
        &mut self,
        column_names: &[&str],
        groups: impl Iterator<Item = Result<Vec<T>, Error>>,
    ) -> Result<(), Error>
    where
        DefaultEncoder: EncodeColumns<T>,
    {
        let path = &self.path;
        let context = || format!("writing parquet file: {}", path.display());
        let file = File::create(path)
            .map_err(Error::from)
            .err_context(|| format!("creating parquet file: {}", path.display()))?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .set_max_row_group_row_count(Some(self.rows_per_group))
            .build();
        let mut file = Some(file);
        let mut writer: Option<ArrowWriter<File>> = None;
        let mut write = |group: Vec<T>| -> Result<(), Error> {
            let (fields, arrays) = DefaultEncoder::encode_all(column_names, group);
            let batch = RecordBatch::try_new(Arc::new(arrowd::Schema::new(fields)), arrays)
                .map_err(Error::Arrow)?;
            if writer.is_none() {
                writer = Some(
                    ArrowWriter::try_new(file.take().unwrap(), batch.schema(), Some(props.clone()))
                        .map_err(Error::Parquet)
                        .err_context(context)?,
                );
            }
            let wtr = writer.as_mut().unwrap();
            wtr.write(&batch)
                .and_then(|()| wtr.flush())
                .map_err(Error::Parquet)
                .err_context(context)
        };
        let mut wrote = false;
        for group in groups {
            write(group?)?;
            wrote = true;
        }
        if !wrote {
            write(Vec::new())?;
        }
        writer
            .unwrap()
            .close()
            .map_err(Error::Parquet)
            .err_context(context)?;
        Ok(())
    }
}

/// The most row groups [`Writer::write_iter_grouped_by`] cuts at key changes. Parquet allows
/// 32767 row groups in a file.
pub const MAX_KEYED_ROW_GROUPS: usize = 30_000;

/// Encoder struct for all the encoding traits. Not for instantiating
pub struct DefaultEncoder {}

/// Decoder struct for all the decoding traits. Not for instantiating
pub struct DefaultDecoder {}

macro_rules! type_as_underscore {
    ($t:ty) => {
        _
    };
}

/// Encoding impl for tuples
macro_rules! impl_encode {
    ($($T:ident),+) => {
        impl <$($T),+>
        EncodeColumns<($($T,)+)> for DefaultEncoder
        where
            $(
                $T: Clone,
            )+
            $(
                Self: EncodeColumn<$T>,
            )+
        {
            #[allow(non_snake_case, unused_assignments)]
            fn encode_all(
                names: &[&str],
                data: Vec<($($T,)+)>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                paste!{
                    let mut i = 0;
                    let mut fvec = Vec::new();
                    let mut cvec = Vec::new();
                    let ($([<c_ $T>],)+): ($(Vec<type_as_underscore!($T)>,)+) = multiunzip(data);
                    $(
                        let ([<f_ $T>], [<c_ $T>]) = Self::encode_column(names[i], [<c_ $T>]);
                        fvec.extend([<f_ $T>]);
                        cvec.extend([<c_ $T>]);
                        i += 1;
                    )+
                    return (fvec, cvec);
                }
            }
        }
        impl <$($T),+>
        DecodeColumns<($($T,)+)> for DefaultDecoder
        where
            $(
                $T: Clone,
            )+
            $(
                Self: DecodeColumn<$T>,
            )+
        {
            #[allow(non_snake_case, unused_assignments)]
            fn decode_tuples(names: &[&str], batch: &RecordBatch) -> Vec<($($T,)+)> {
                let mut result = Vec::new();
                paste!{
                    let mut index = 0;
                    $(
                        let [<arr_ $T>] = <DefaultDecoder as DecodeColumn::<$T>>::into_decode_array(
                            names[index], &batch);
                        index += 1;
                    )+
                    for ($([<v_ $T>]),+) in izip![$([<arr_ $T>]),+] {
                        result.push(($([<v_ $T>],)+));
                    }
                }
                result.shrink_to_fit();
                result
            }
        }
    }
}

impl_encode!(T0);
impl_encode!(T0, T1);
impl_encode!(T0, T1, T2);
impl_encode!(T0, T1, T2, T3);
impl_encode!(T0, T1, T2, T3, T4);
impl_encode!(T0, T1, T2, T3, T5, T6);
impl_encode!(T0, T1, T2, T3, T5, T6, T7);
impl_encode!(T0, T1, T2, T3, T5, T6, T7, T8);
impl_encode!(T0, T1, T2, T3, T5, T6, T7, T8, T9);
impl_encode!(T0, T1, T2, T3, T5, T6, T7, T8, T9, T10);
impl_encode!(T0, T1, T2, T3, T5, T6, T7, T8, T9, T10, T11);

/// Impls to decode array of primitives and an array of primitive options
macro_rules! impl_encode_primitive {
    ($rustty:ty, $arraycon:path, $parquetty:ty, $arrowty:expr) => {
        impl EncodeColumn<$rustty> for DefaultEncoder {
            #[inline]
            fn encode_column(name: &str, col: Vec<$rustty>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                let array = Arc::new($arraycon(col));
                let f = arrowd::Field::new(name, $arrowty, false);
                (vec![f], vec![array])
            }
        }
        impl DecodeColumn<$rustty> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = $rustty> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
                    .into_iter()
                    .map(|o| o.unwrap())
            }
        }
        impl EncodeColumn<Option<$rustty>> for DefaultEncoder {
            #[inline]
            fn encode_column(
                name: &str,
                col: Vec<Option<$rustty>>,
            ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                let array = Arc::new($arraycon(col));
                let f = arrowd::Field::new(name, $arrowty, true);
                (vec![f], vec![array])
            }
        }
        impl DecodeColumn<Option<$rustty>> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = Option<$rustty>> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
            }
        }
    };
}

impl_encode_primitive!(
    bool,
    BooleanArray::from,
    BooleanArray,
    arrowd::DataType::Boolean
);
impl_encode_primitive!(i8, Int8Array::from, Int8Array, arrowd::DataType::Int8);
impl_encode_primitive!(u8, UInt8Array::from, UInt8Array, arrowd::DataType::UInt8);
impl_encode_primitive!(i16, Int16Array::from, Int16Array, arrowd::DataType::Int16);
impl_encode_primitive!(
    u16,
    UInt16Array::from,
    UInt16Array,
    arrowd::DataType::UInt16
);
impl_encode_primitive!(i32, Int32Array::from, Int32Array, arrowd::DataType::Int32);
impl_encode_primitive!(
    u32,
    UInt32Array::from,
    UInt32Array,
    arrowd::DataType::UInt32
);
impl_encode_primitive!(i64, Int64Array::from, Int64Array, arrowd::DataType::Int64);
impl_encode_primitive!(
    u64,
    UInt64Array::from,
    UInt64Array,
    arrowd::DataType::UInt64
);

macro_rules! impl_encode_column {
    ($rustty:ty, $arraycon:path, $parquetty:ty, $fieldty:expr) => {
        impl EncodeColumn<$rustty> for DefaultEncoder {
            #[inline]
            fn encode_column(name: &str, col: Vec<$rustty>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                let array = Arc::new($arraycon(col.iter().map(|s| s.as_ref()).collect_vec()));
                let f = arrowd::Field::new(name, $fieldty, false);
                (vec![f], vec![array])
            }
        }
        impl DecodeColumn<$rustty> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = $rustty> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
                    .into_iter()
                    .map(|o| o.unwrap().into())
            }
        }

        impl EncodeColumn<Option<$rustty>> for DefaultEncoder {
            #[inline]
            fn encode_column(
                name: &str,
                col: Vec<Option<$rustty>>,
            ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                let array = Arc::new($arraycon(
                    col.iter()
                        .map(|s| s.as_ref().map(|s| s.as_ref()))
                        .collect_vec(),
                ));
                let f = arrowd::Field::new(name, $fieldty, true);
                (vec![f], vec![array])
            }
        }
        impl DecodeColumn<Option<$rustty>> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = Option<$rustty>> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
                    .into_iter()
                    .map(|s| s.map(|v| v.into()))
            }
        }
    };
}

impl_encode_column!(
    String,
    GenericStringArray::<i64>::from,
    GenericStringArray<i64>,
    arrowd::DataType::LargeUtf8
);
impl_encode_column!(
    ArcIntern<str>,
    GenericStringArray::<i64>::from,
    GenericStringArray<i64>,
    arrowd::DataType::LargeUtf8
);
impl_encode_column!(
    Str,
    GenericStringArray::<i64>::from,
    GenericStringArray<i64>,
    arrowd::DataType::LargeUtf8
);

macro_rules! impl_encode_newtype {
    ($newty:path, $rustty:ty, $parquetty:ty) => {
        impl EncodeColumn<$newty> for DefaultEncoder {
            #[inline]
            fn encode_column(name: &str, col: Vec<$newty>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                <Self as EncodeColumn<$rustty>>::encode_column(
                    name,
                    col.into_iter().map(|s| s.0.clone()).collect_vec(),
                )
            }
        }
        impl DecodeColumn<$newty> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = $newty> {
                <Self as DecodeColumn<$rustty>>::into_decode_array(name, batch)
                    .into_iter()
                    .map(|o| $newty(o.into()))
            }
        }
        impl EncodeColumn<Option<$newty>> for DefaultEncoder {
            #[inline]
            fn encode_column(
                name: &str,
                col: Vec<Option<$newty>>,
            ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                <Self as EncodeColumn<Option<$rustty>>>::encode_column(
                    name,
                    col.into_iter()
                        .map(|s| s.as_ref().map(|o| o.0.clone()))
                        .collect_vec(),
                )
            }
        }
        impl DecodeColumn<Option<$newty>> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = Option<$newty>> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
                    .into_iter()
                    .map(|s| s.map(|v| $newty(v.into())))
            }
        }
    };
}

// A `Path` is stored as its canonical string (`Path::to_dot_string`), which `Path::parse` reads
// back. Both go straight between the string column and the path, so encoding interns no `Str`.
impl EncodeColumn<facts::Path> for DefaultEncoder {
    #[inline]
    fn encode_column(name: &str, col: Vec<facts::Path>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        let strings: Vec<String> = col.iter().map(facts::Path::to_dot_string).collect();
        <Self as EncodeColumn<String>>::encode_column(name, strings)
    }
}

impl DecodeColumn<facts::Path> for DefaultDecoder {
    #[inline]
    fn into_decode_array(name: &str, batch: &RecordBatch) -> impl IntoIterator<Item = facts::Path> {
        batch
            .column_by_name(name)
            .unwrap()
            .as_any()
            .downcast_ref::<GenericStringArray<i64>>()
            .unwrap()
            .iter()
            .map(move |s| {
                let s = s.unwrap();
                if s.is_empty() {
                    return facts::Path::empty();
                }
                s.parse().unwrap_or_else(|e| {
                    panic!(
                        "corrupt access path {s:?} in fact column {name:?}: {e}\n\
                         this index was written by an incompatible build; re-run `ctadl index`"
                    )
                })
            })
            .collect_vec()
    }
}
impl_encode_newtype!(facts::Function, Str, GenericStringArray<i64>);
impl_encode_newtype!(facts::FormalIndex, i16, Int16Array);
impl_encode_newtype!(facts::GlobalId, Str, GenericStringArray<i64>);
impl_encode_newtype!(source_info::FileSpanId, u32, UInt32Array);
impl_encode_newtype!(facts::ImportId, u32, UInt32Array);

macro_rules! impl_encode_newtype_field {
    ($newty:path, $newcon:path, $rustty:ty, $fld:ident, $parquetty:ty) => {
        impl EncodeColumn<$newty> for DefaultEncoder {
            #[inline]
            fn encode_column(name: &str, col: Vec<$newty>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                <Self as EncodeColumn<$rustty>>::encode_column(
                    name,
                    col.into_iter().map(|s| s.$fld.clone()).collect_vec(),
                )
            }
        }
        impl DecodeColumn<$newty> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = $newty> {
                <Self as DecodeColumn<$rustty>>::into_decode_array(name, batch)
                    .into_iter()
                    .map(|o| $newcon(o.into()))
            }
        }
        impl EncodeColumn<Option<$newty>> for DefaultEncoder {
            #[inline]
            fn encode_column(
                name: &str,
                col: Vec<Option<$newty>>,
            ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                <Self as EncodeColumn<Option<$rustty>>>::encode_column(
                    name,
                    col.into_iter()
                        .map(|s| s.as_ref().map(|o| o.$fld.clone()))
                        .collect_vec(),
                )
            }
        }
        impl DecodeColumn<Option<$newty>> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = Option<$newty>> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
                    .into_iter()
                    .map(|s| s.map(|v| $newcon(v.into())))
            }
        }
    };
}

// Use From impl in both directions
macro_rules! impl_encode_newtype_from {
    ($newty:path, $rustty:ty, $parquetty:ty) => {
        impl EncodeColumn<$newty> for DefaultEncoder {
            #[inline]
            fn encode_column(name: &str, col: Vec<$newty>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                <Self as EncodeColumn<$rustty>>::encode_column(
                    name,
                    col.into_iter().map(|s| s.into()).collect_vec(),
                )
            }
        }
        impl DecodeColumn<$newty> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = $newty> {
                <Self as DecodeColumn<$rustty>>::into_decode_array(name, batch)
                    .into_iter()
                    .map(|o| o.into())
            }
        }
        impl EncodeColumn<Option<$newty>> for DefaultEncoder {
            #[inline]
            fn encode_column(
                name: &str,
                col: Vec<Option<$newty>>,
            ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
                <Self as EncodeColumn<Option<$rustty>>>::encode_column(
                    name,
                    col.into_iter()
                        .map(|s| s.as_ref().map(|o| (*o).into()))
                        .collect_vec(),
                )
            }
        }
        impl DecodeColumn<Option<$newty>> for DefaultDecoder {
            #[inline]
            fn into_decode_array(
                name: &str,
                batch: &RecordBatch,
            ) -> impl IntoIterator<Item = Option<$newty>> {
                batch
                    .column_by_name(name)
                    .unwrap()
                    .as_any()
                    .downcast_ref::<$parquetty>()
                    .unwrap()
                    .into_iter()
                    .map(|s| s.map(|v| v.into()))
            }
        }
    };
}

impl_encode_newtype_field!(
    facts::FunctionId,
    facts::FunctionId::new,
    u32,
    id,
    UInt32Array
);
impl_encode_newtype_field!(facts::InsnId, facts::InsnId::new, u64, id, UInt64Array);
impl_encode_newtype_from!(ctadl_ir::BasicBlockIdx, u32, UInt32Array);
impl_encode_newtype_from!(ctadl_ir::StatementIdx, u32, UInt32Array);

impl EncodeColumn<facts::FormalType> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::FormalType>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        use facts::FormalType::*;
        <Self as EncodeColumn<u8>>::encode_column(
            name,
            col.into_iter()
                .map(|t| match t {
                    SharedFields => 0,
                    Shared => 1,
                    Copied => 2,
                })
                .collect_vec(),
        )
    }
}

impl DecodeColumn<facts::FormalType> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::FormalType> {
        use facts::FormalType::*;
        <Self as DecodeColumn<u8>>::into_decode_array(name, batch)
            .into_iter()
            .map(|i| match i {
                0 => SharedFields,
                1 => Shared,
                2 => Copied,
                _ => panic!("bad encoding of FormalType"),
            })
    }
}

impl EncodeColumn<facts::PackedInsnSiteId> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::PackedInsnSiteId>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<[u8; 8]>>::encode_column(
            name,
            col.into_iter().map(|s| s.0).collect_vec(),
        )
    }
}

impl EncodeColumn<Option<facts::PackedInsnSiteId>> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<Option<facts::PackedInsnSiteId>>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<Option<[u8; 8]>>>::encode_column(
            name,
            col.into_iter().map(|s| s.map(|s| s.0)).collect_vec(),
        )
    }
}

impl EncodeColumn<[u8; 8]> for DefaultEncoder {
    #[inline]
    fn encode_column(name: &str, col: Vec<[u8; 8]>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        let fld = vec![arrowd::Field::new(name, arrowd::DataType::UInt64, false)];
        let arr: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from_iter_values(
            col.into_iter().map(u64::from_be_bytes),
        ))];
        (fld, arr)
    }
}

impl EncodeColumn<Option<[u8; 8]>> for DefaultEncoder {
    #[inline]
    fn encode_column(name: &str, col: Vec<Option<[u8; 8]>>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        let fld = vec![arrowd::Field::new(name, arrowd::DataType::UInt64, true)];
        let arr: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from(
            col.into_iter()
                .map(|arr| arr.map(u64::from_be_bytes))
                .collect_vec(),
        ))];
        (fld, arr)
    }
}

impl DecodeColumn<[u8; 8]> for DefaultDecoder {
    #[inline]
    fn into_decode_array(name: &str, batch: &RecordBatch) -> impl IntoIterator<Item = [u8; 8]> {
        batch
            .column_by_name(name)
            .unwrap()
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .into_iter()
            .map(|i| u64::to_be_bytes(i.unwrap()))
    }
}

impl DecodeColumn<Option<[u8; 8]>> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = Option<[u8; 8]>> {
        batch
            .column_by_name(name)
            .unwrap()
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .into_iter()
            .map(|opt| opt.map(u64::to_be_bytes))
    }
}

impl DecodeColumn<facts::PackedInsnSiteId> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::PackedInsnSiteId> {
        <Self as DecodeColumn<[u8; 8]>>::into_decode_array(name, batch)
            .into_iter()
            .map(facts::PackedInsnSiteId)
    }
}

impl DecodeColumn<Option<facts::PackedInsnSiteId>> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = Option<facts::PackedInsnSiteId>> {
        <Self as DecodeColumn<Option<[u8; 8]>>>::into_decode_array(name, batch)
            .into_iter()
            .map(|o| o.map(facts::PackedInsnSiteId))
    }
}

// A `CallDispatchKey` spans three arrow columns: a `<name>_tag` byte (0 = Java, 1 = C,
// 2 = Lua, 3 = Cil), and nullable `<name>_name` / `<name>_desc` symbol columns (both present
// iff the variant is `Java` or `Cil`, both null for `C`; `Lua` populates only `<name>_name`).
impl EncodeColumn<facts::CallDispatchKey> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::CallDispatchKey>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        use facts::CallDispatchKeyKind::*;
        let tag_column_name = name.to_owned() + "_tag";
        let name_column_name = name.to_owned() + "_name";
        let desc_column_name = name.to_owned() + "_desc";
        let (mut fields, mut arrays) = <Self as EncodeColumn<u8>>::encode_column(
            &tag_column_name,
            col.iter()
                .map(|c| match c.kind() {
                    Java(_, _) => 0u8,
                    C => 1,
                    Lua(_) => 2,
                    Cil(_, _) => 3,
                })
                .collect_vec(),
        );
        let (name_fields, name_arrays) =
            <Self as EncodeColumn<Option<ArcIntern<str>>>>::encode_column(
                &name_column_name,
                col.iter()
                    .map(|c| match c.kind() {
                        Java(n, _) | Lua(n) | Cil(n, _) => Some(n.clone()),
                        C => None,
                    })
                    .collect_vec(),
            );
        let (desc_fields, desc_arrays) =
            <Self as EncodeColumn<Option<ArcIntern<str>>>>::encode_column(
                &desc_column_name,
                col.iter()
                    .map(|c| match c.kind() {
                        Java(_, d) | Cil(_, d) => Some(d.clone()),
                        C | Lua(_) => None,
                    })
                    .collect_vec(),
            );
        fields.extend(name_fields);
        arrays.extend(name_arrays);
        fields.extend(desc_fields);
        arrays.extend(desc_arrays);
        (fields, arrays)
    }
}

impl DecodeColumn<facts::CallDispatchKey> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::CallDispatchKey> {
        use facts::CallDispatchKeyKind::*;
        let tag_column_name = name.to_owned() + "_tag";
        let name_column_name = name.to_owned() + "_name";
        let desc_column_name = name.to_owned() + "_desc";
        let tags = <Self as DecodeColumn<u8>>::into_decode_array(&tag_column_name, batch);
        let names = <Self as DecodeColumn<Option<ArcIntern<str>>>>::into_decode_array(
            &name_column_name,
            batch,
        );
        let descs = <Self as DecodeColumn<Option<ArcIntern<str>>>>::into_decode_array(
            &desc_column_name,
            batch,
        );
        izip![tags, names, descs]
            .map(|(tag, name, desc)| match tag {
                0 => Java(
                    name.expect("Java CallDispatchKey missing simple name"),
                    desc.expect("Java CallDispatchKey missing descriptor"),
                ),
                1 => C,
                2 => Lua(name.expect("Lua CallDispatchKey missing method name")),
                3 => Cil(
                    name.expect("Cil CallDispatchKey missing simple name"),
                    desc.expect("Cil CallDispatchKey missing descriptor"),
                ),
                _ => panic!("bad encoding of CallDispatchKey"),
            })
            .map(facts::CallDispatchKey::from)
            .collect_vec()
    }
}

impl EncodeColumn<facts::IntentPairKind> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::IntentPairKind>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<u8>>::encode_column(
            name,
            col.into_iter()
                .map(|kind| match kind {
                    facts::IntentPairKind::Explicit => 0,
                    facts::IntentPairKind::Implicit => 1,
                })
                .collect_vec(),
        )
    }
}

impl DecodeColumn<facts::IntentPairKind> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::IntentPairKind> {
        <Self as DecodeColumn<u8>>::into_decode_array(name, batch)
            .into_iter()
            .map(|tag| match tag {
                0 => facts::IntentPairKind::Explicit,
                1 => facts::IntentPairKind::Implicit,
                _ => panic!("bad encoding of IntentPairKind"),
            })
    }
}

impl EncodeColumn<facts::IntentKind> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::IntentKind>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        use facts::IntentKind::*;
        <Self as EncodeColumn<u8>>::encode_column(
            name,
            col.into_iter()
                .map(|kind| match kind {
                    Activity => 0,
                    Receiver => 1,
                    StartedService => 2,
                    BoundService => 3,
                })
                .collect_vec(),
        )
    }
}

impl DecodeColumn<facts::IntentKind> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::IntentKind> {
        use facts::IntentKind::*;
        <Self as DecodeColumn<u8>>::into_decode_array(name, batch)
            .into_iter()
            .map(|tag| match tag {
                0 => Activity,
                1 => Receiver,
                2 => StartedService,
                3 => BoundService,
                _ => panic!("bad encoding of IntentKind"),
            })
    }
}

impl EncodeColumn<facts::PackedCallArg> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::PackedCallArg>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<[u8; 8]>>::encode_column(
            name,
            col.into_iter().map(|s| *s).collect_vec(),
        )
    }
}

impl EncodeColumn<Option<facts::PackedCallArg>> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<Option<facts::PackedCallArg>>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<Option<[u8; 8]>>>::encode_column(
            name,
            col.into_iter().map(|s| s.map(|s| *s)).collect_vec(),
        )
    }
}

impl DecodeColumn<facts::PackedCallArg> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::PackedCallArg> {
        <Self as DecodeColumn<[u8; 8]>>::into_decode_array(name, batch)
            .into_iter()
            .map(facts::PackedCallArg)
    }
}

impl DecodeColumn<Option<facts::PackedCallArg>> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = Option<facts::PackedCallArg>> {
        <Self as DecodeColumn<Option<[u8; 8]>>>::into_decode_array(name, batch)
            .into_iter()
            .map(|o| o.map(facts::PackedCallArg))
    }
}

type FlowVariableRefEncoding = (
    Option<Str>,
    Option<facts::FormalIndex>,
    Option<facts::PackedCallArg>,
);

/// A [`facts::Root`] is two columns, `<name>_index` (the formal, null for a global) and
/// `<name>_global` (the global's name, null for a formal).
impl EncodeColumn<facts::Root> for DefaultEncoder {
    #[inline]
    fn encode_column(name: &str, col: Vec<facts::Root>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        let names = [format!("{name}_index"), format!("{name}_global")];
        let names = names.iter().map(|s| s.as_ref()).collect_vec();
        <Self as EncodeColumns<(Option<facts::FormalIndex>, Option<Str>)>>::encode_all(
            &names,
            col.into_iter()
                .map(|r| match r.kind() {
                    facts::RootKind::Formal(i) => (Some(i), None),
                    facts::RootKind::Global(g) => (None, Some(g.name())),
                })
                .collect_vec(),
        )
    }
}

impl DecodeColumn<facts::Root> for DefaultDecoder {
    #[inline]
    fn into_decode_array(name: &str, batch: &RecordBatch) -> impl IntoIterator<Item = facts::Root> {
        let names = [format!("{name}_index"), format!("{name}_global")];
        let names = names.iter().map(|s| s.as_ref()).collect_vec();
        <Self as DecodeColumns<(Option<facts::FormalIndex>, Option<Str>)>>::decode_tuples(
            &names, batch,
        )
        .into_iter()
        .map(|cols| match cols {
            (Some(i), None) => facts::Root::formal(i),
            (None, Some(g)) => facts::Root::global(facts::GlobalId(g)),
            _ => panic!("a root is a formal or a global"),
        })
        .collect_vec()
    }
}

impl EncodeColumn<facts::FlowVariable> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::FlowVariable>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        use facts::FlowVariableKind::*;
        let tag_column_name = name.to_owned() + "_tag";
        let ref_column_name = name.to_owned() + "_ref";
        let ref_col_names = [
            (ref_column_name.to_owned() + "_name"),
            (ref_column_name.to_owned() + "_ind"),
            (ref_column_name.to_owned() + "_arg"),
        ];
        let ref_col_names = ref_col_names.iter().map(|s| s.as_ref()).collect_vec();

        let (mut fields, mut arrays) = <Self as EncodeColumn<u8>>::encode_column(
            &tag_column_name,
            col.iter()
                .map(|r| match r.kind() {
                    Local(_) => 0,
                    Formal(_) => 1,
                    CallArg(_) => 2,
                    Uninit => 3,
                    Global(_) => 4,
                })
                .collect_vec(),
        );
        let (data_fields, data_arrays) =
            <Self as EncodeColumns<FlowVariableRefEncoding>>::encode_all(
                &ref_col_names,
                col.into_iter()
                    .map(|r| match r.kind() {
                        Local(name) => (Some(name), None, None),
                        Formal(ind) => (None, Some(ind), None),
                        CallArg(packed) => (None, None, Some(packed)),
                        Uninit => (None, None, None),
                        Global(g) => (Some(g.name()), None, None),
                    })
                    .collect_vec(),
            );
        fields.extend(data_fields);
        arrays.extend(data_arrays);
        (fields, arrays)
    }
}

impl DecodeColumn<facts::FlowVariable> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::FlowVariable> {
        let tag_column_name: &'static str = Box::leak(format!("{name}_tag").into_boxed_str());
        let ref_column_name = name.to_owned() + "_ref";
        let ref_col_names = [
            (ref_column_name.to_owned() + "_name"),
            (ref_column_name.to_owned() + "_ind"),
            (ref_column_name.to_owned() + "_arg"),
        ];
        let ref_col_names = ref_col_names.iter().map(|s| s.as_ref()).collect_vec();
        let tag_iter = <Self as DecodeColumn<u8>>::into_decode_array(tag_column_name, batch);
        let val_iter =
            <Self as DecodeColumns<FlowVariableRefEncoding>>::decode_tuples(&ref_col_names, batch);
        izip![tag_iter, val_iter].map(|(tag, val)| match (tag, val) {
            (0, cols) => facts::FlowVariable::local(cols.0.unwrap()),
            (1, cols) => facts::FlowVariable::formal_index(cols.1.unwrap()),
            (2, cols) => facts::FlowVariable::call_arg_packed(cols.2.expect("call_arg")),
            (3, _) => facts::FlowVariable::default(),
            (4, cols) => facts::FlowVariable::global(facts::GlobalId(cols.0.unwrap())),
            _ => panic!("Bad flow variable"),
        })
    }
}
