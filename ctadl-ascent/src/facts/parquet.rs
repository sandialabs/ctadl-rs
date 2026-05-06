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
    arrow::arrow_reader::ParquetRecordBatchReaderBuilder, basic::Compression,
    file::properties::WriterProperties,
};
use paste::paste;

use crate::error::Error;
use crate::facts;
use crate::query_engine;

type Str = ArcIntern<str>;

// maybe move to these
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

impl Reader {
    /// Reads from the given path
    pub fn new<P: AsRef<std::path::Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }

    /// Read a parquet file into a vec. The vec rows are read using the given column names
    pub fn read_vec<T>(&self, column_names: &[&str]) -> Result<Vec<T>, Error>
    where
        DefaultDecoder: DecodeColumns<T>,
    {
        let file = File::open(&self.path).map_err(Error::Io)?;
        let rdr = ParquetRecordBatchReaderBuilder::try_new(file)
            .and_then(|b| b.build())
            .map_err(Error::Parquet)?;
        let mut result = Vec::new();
        for batch_result in rdr {
            let batch = batch_result.unwrap();
            result.extend(DefaultDecoder::decode_tuples(column_names, &batch));
        }
        result.shrink_to_fit();
        Ok(result)
    }
}

pub struct Writer {
    path: std::path::PathBuf,
}

impl Writer {
    /// Writes to the given path
    pub fn new<P: AsRef<std::path::Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }

    /// Write the vec to a parquet file. The vec rows are written using the given column names
    pub fn write_vec<T>(&mut self, column_names: &[&str], data: Vec<T>) -> Result<(), Error>
    where
        DefaultEncoder: EncodeColumns<T>,
    {
        let (fields, arrays) = DefaultEncoder::encode_all(column_names, data);
        let batch = RecordBatch::try_new(Arc::new(arrowd::Schema::new(fields)), arrays)
            .map_err(Error::Arrow)?;

        let file = File::create(&self.path).map_err(Error::Io)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        ArrowWriter::try_new(file, batch.schema(), Some(props))
            .and_then(|mut wtr| {
                wtr.write(&batch)?;
                wtr.close()
            })
            .map_err(Error::Parquet)?;
        Ok(())
    }
}

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

// Custom encoding for Path since it's now VecDeque<mir::FieldAccess> instead of Str
impl EncodeColumn<facts::Path> for DefaultEncoder {
    #[inline]
    fn encode_column(name: &str, col: Vec<facts::Path>) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        // Convert each Path to escaped string format for parquet storage
        let strings: Vec<Str> = col
            .into_iter()
            .map(|path| path.to_dot_string().into())
            .collect();
        <Self as EncodeColumn<Str>>::encode_column(name, strings)
    }
}

impl DecodeColumn<facts::Path> for DefaultDecoder {
    #[inline]
    fn into_decode_array(name: &str, batch: &RecordBatch) -> impl IntoIterator<Item = facts::Path> {
        <Self as DecodeColumn<Str>>::into_decode_array(name, batch)
            .into_iter()
            .map(|s| {
                if s.is_empty() {
                    facts::Path::empty()
                } else {
                    // Parse the string representation back to Path
                    s.parse().unwrap_or_else(|_| facts::Path::empty())
                }
            })
    }
}
impl_encode_newtype!(facts::Function, Str, GenericStringArray<i64>);
impl_encode_newtype!(facts::Label, Str, GenericStringArray<i64>);
impl_encode_newtype!(facts::Index, i16, Int16Array);
impl_encode_newtype!(facts::FormalIndex, facts::Index, Int16Array);
impl_encode_newtype!(source_info::FileSpanId, u32, UInt32Array);

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
                    ByVal => 0,
                    ByRef => 1,
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
                0 => ByVal,
                1 => ByRef,
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
    /// TaintDirection is encoded as a boolean. 'true' is the forward direction, 'false' is the
    /// backward direction.
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

impl EncodeColumn<facts::TaintState> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::TaintState>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<bool>>::encode_column(
            name,
            col.into_iter()
                .map(|s| match s {
                    facts::TaintState::Free => true,
                    facts::TaintState::Restricted => false,
                })
                .collect_vec(),
        )
    }
}

impl DecodeColumn<facts::TaintState> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::TaintState> {
        <Self as DecodeColumn<bool>>::into_decode_array(name, batch)
            .into_iter()
            .map(|s| match s {
                true => facts::TaintState::Free,
                false => facts::TaintState::Restricted,
            })
    }
}

impl EncodeColumn<facts::TaintDirection> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::TaintDirection>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        <Self as EncodeColumn<bool>>::encode_column(
            name,
            col.into_iter()
                .map(|s| match s {
                    facts::TaintDirection::Forward => true,
                    facts::TaintDirection::Backward => false,
                })
                .collect_vec(),
        )
    }
}

impl DecodeColumn<facts::TaintDirection> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = facts::TaintDirection> {
        <Self as DecodeColumn<bool>>::into_decode_array(name, batch)
            .into_iter()
            .map(|s| match s {
                true => facts::TaintDirection::Forward,
                false => facts::TaintDirection::Backward,
            })
    }
}

// type QueryEndpointEncoding = (

impl EncodeColumn<query_engine::QueryEndpoint> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<query_engine::QueryEndpoint>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        DefaultEncoder::encode_all(
            &[
                &(name.to_owned() + "_infunc"),
                &(name.to_owned() + "_var"),
                &(name.to_owned() + "_path"),
                &(name.to_owned() + "_label"),
                &(name.to_owned() + "_direction"),
            ],
            col.into_iter()
                .map(|e| (e.infunc, e.vertex.0, e.vertex.1, e.label, e.direction))
                .collect(),
        )
    }
}

impl DecodeColumn<query_engine::QueryEndpoint> for DefaultDecoder {
    #[inline]
    fn into_decode_array(
        name: &str,
        batch: &RecordBatch,
    ) -> impl IntoIterator<Item = query_engine::QueryEndpoint> {
        DefaultDecoder::decode_tuples(
            &[
                &(name.to_owned() + "_infunc"),
                &(name.to_owned() + "_var"),
                &(name.to_owned() + "_path"),
                &(name.to_owned() + "_label"),
                &(name.to_owned() + "_direction"),
            ],
            batch,
        )
        .into_iter()
        .map(
            |(infunc, var, path, label, direction)| query_engine::QueryEndpoint {
                infunc,
                vertex: facts::FlowVertex(var, path),
                label,
                direction,
            },
        )
    }
}

type FlowVariableRefEncoding = (
    Option<Str>,
    Option<facts::FormalIndex>,
    Option<facts::PackedInsnSiteId>,
    Option<facts::FormalIndex>,
);

impl EncodeColumn<facts::FlowVariable> for DefaultEncoder {
    #[inline]
    fn encode_column(
        name: &str,
        col: Vec<facts::FlowVariable>,
    ) -> (Vec<arrowd::Field>, Vec<ArrayRef>) {
        use facts::FlowVariable::{CallArg, Formal, Local};
        let tag_column_name = name.to_owned() + "_tag";
        let ref_column_name = name.to_owned() + "_ref";
        let ref_col_names = [
            (ref_column_name.to_owned() + "_name"),
            (ref_column_name.to_owned() + "_ind"),
            (ref_column_name.to_owned() + "_arg_site_id"),
            (ref_column_name.to_owned() + "_arg_ind"),
        ];
        let ref_col_names = ref_col_names.iter().map(|s| s.as_ref()).collect_vec();

        let (mut fields, mut arrays) = <Self as EncodeColumn<u8>>::encode_column(
            &tag_column_name,
            col.iter()
                .map(|r| match r {
                    Local(_) => 0,
                    Formal(_) => 1,
                    CallArg { .. } => 2,
                    _ => panic!("Invalid flow variable: {r:?}"),
                })
                .collect_vec(),
        );
        let (data_fields, data_arrays) =
            <Self as EncodeColumns<FlowVariableRefEncoding>>::encode_all(
                &ref_col_names,
                col.into_iter()
                    .map(|r| match r {
                        Local(name) => (Some(name), None, None, None),
                        Formal(ind) => (None, Some(ind), None, None),
                        CallArg { id, formal } => (None, None, Some(id), Some(formal)),
                        _ => panic!("Invalid flow variable: {r:?}"),
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
        use facts::FlowVariable::{CallArg, Formal, Local};
        let tag_column_name: &'static str = Box::leak(format!("{name}_tag").into_boxed_str());
        let ref_column_name = name.to_owned() + "_ref";
        let ref_col_names = [
            (ref_column_name.to_owned() + "_name"),
            (ref_column_name.to_owned() + "_ind"),
            (ref_column_name.to_owned() + "_arg_site_id"),
            (ref_column_name.to_owned() + "_arg_ind"),
        ];
        let ref_col_names = ref_col_names.iter().map(|s| s.as_ref()).collect_vec();
        let tag_iter = <Self as DecodeColumn<u8>>::into_decode_array(tag_column_name, batch);
        let val_iter =
            <Self as DecodeColumns<FlowVariableRefEncoding>>::decode_tuples(&ref_col_names, batch);
        izip![tag_iter, val_iter].map(|(tag, val)| match (tag, val) {
            (0, cols) => Local(cols.0.unwrap()),
            (1, cols) => Formal(cols.1.unwrap()),
            (2, cols) => CallArg {
                id: cols.2.expect("site_id"),
                formal: cols.3.unwrap(),
            },
            _ => panic!("Bad flow variable"),
        })
    }
}
