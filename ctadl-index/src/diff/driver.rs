//! [`diff_index`]: the comparison of two index directories.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path as FsPath;

use crate::facts::schema::Table;
use crate::index_engine::dump::Cx;

use super::insn::{InsnMap, SignatureInterner};
use super::names::{FunctionNames, ImportNames};
use super::report::{Filters, Report, UnknownFile};
use super::table::{FunctionFilter, LoadCx, TablePair, diff_one, needs_insns};
use super::{Error, Side, error_chain};

/// What to compare and how much to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The most differing rows printed per table and side.
    pub limit: usize,
    /// Compare only these tables, by name (`call`) or file name (`call.parquet`); all if empty.
    pub tables: Vec<String>,
    /// Compare only the rows about a function whose name contains this. A row about no
    /// function, such as one of `paths`, is left out.
    pub function: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            limit: 20,
            tables: Vec::new(),
            function: None,
        }
    }
}

/// Compares the index directories `a` and `b`, which the caller has checked are in the format
/// this build reads.
///
/// Every table either side holds is compared, one at a time; a table on one side only is a
/// difference. A `.parquet` file no table is stored in is listed in the report and not
/// compared. An error names the table and the side it is about.
pub fn diff_index(a: &FsPath, b: &FsPath, opts: &Options) -> Result<Report, Error> {
    let files = [parquet_files(a, Side::A)?, parquet_files(b, Side::B)?];
    let present = |side: usize, t: Table| files[side].contains(t.filename());

    let mut unknown: BTreeMap<&str, UnknownFile> = BTreeMap::new();
    for (side, names) in files.iter().enumerate() {
        for f in names.iter().filter(|f| Table::from_filename(f).is_none()) {
            let u = unknown.entry(f).or_insert_with(|| UnknownFile {
                file: f.clone(),
                in_a: false,
                in_b: false,
            });
            if side == 0 {
                u.in_a = true;
            } else {
                u.in_b = true;
            }
        }
    }

    let selected = select_tables(&opts.tables)?;
    let filters = Filters {
        tables: if opts.tables.is_empty() {
            Vec::new()
        } else {
            selected.iter().map(|t| t.name().to_string()).collect()
        },
        function: opts.function.clone(),
    };

    let fns = [
        FunctionNames::load(a, Side::A, present(0, Table::FunctionId))?,
        FunctionNames::load(b, Side::B, present(1, Table::FunctionId))?,
    ];
    let imports = [
        ImportNames::load(a, Side::A, present(0, Table::ImportId))?,
        ImportNames::load(b, Side::B, present(1, Table::ImportId))?,
    ];
    let mut sigs = SignatureInterner::new();
    let insns = if selected.iter().any(|&t| needs_insns(t)) {
        [
            InsnMap::build(a, Side::A, |t| present(0, t), &fns[0], &mut sigs)?,
            InsnMap::build(b, Side::B, |t| present(1, t), &fns[1], &mut sigs)?,
        ]
    } else {
        [InsnMap::empty(), InsnMap::empty()]
    };

    let cx = Cx::new(None, Some(&sigs));
    let mut filter = FunctionFilter::new(opts.function.as_deref());
    let mut tables = Vec::new();
    for t in selected {
        let pair = TablePair {
            dirs: [a, b],
            present: [present(0, t), present(1, t)],
            load: [Side::A, Side::B].map(|side| {
                let i = side as usize;
                LoadCx {
                    side,
                    fns: &fns[i],
                    imports: &imports[i],
                    insns: &insns[i],
                }
            }),
            cx: &cx,
            limit: opts.limit,
        };
        if pair.present == [false, false] {
            continue;
        }
        tables.push(diff_one(t, &pair, &mut filter)?);
    }

    Ok(Report {
        a: a.display().to_string(),
        b: b.display().to_string(),
        limit: opts.limit,
        filters,
        not_compared: unknown.into_values().collect(),
        tables,
    })
}

/// The `.parquet` files directly in `dir`, by name.
fn parquet_files(dir: &FsPath, side: Side) -> Result<BTreeSet<String>, Error> {
    let context = |e: std::io::Error| {
        Error::new(format!(
            "listing {side}'s index directory {}: {}",
            dir.display(),
            error_chain(&e)
        ))
    };
    let mut files = BTreeSet::new();
    for entry in std::fs::read_dir(dir).map_err(context)? {
        let entry = entry.map_err(context)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".parquet") && entry.file_type().map_err(context)?.is_file() {
            files.insert(name.to_string());
        }
    }
    Ok(files)
}

/// The tables `names` selects, in name order; every table if it is empty.
fn select_tables(names: &[String]) -> Result<Vec<Table>, Error> {
    let mut tables: Vec<Table> = if names.is_empty() {
        Table::ALL.to_vec()
    } else {
        names
            .iter()
            .map(|n| {
                Table::from_name(n)
                    .or_else(|| Table::from_filename(n))
                    .ok_or_else(|| Error::new(format!("no table is named {n:?}")))
            })
            .collect::<Result<_, _>>()?
    };
    tables.sort_unstable_by_key(|t| t.name());
    tables.dedup();
    Ok(tables)
}
