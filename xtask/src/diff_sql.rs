//! Checks `ctadl diff-index` against SQL that duckdb runs over the same parquet files.
//!
//! After the regression cases run, the indexes they wrote are sorted by path and each is
//! compared with the next. For every table no row of which names an instruction, the counts
//! `diff-index --json` reports (rows on each side; distinct rows only in A, only in B, and on
//! both sides a different number of times) must equal an independent query's: each side's rows
//! grouped by their canonical columns with `count(*)` (function and import ids joined to names,
//! span ids left out), then a full outer join of the two. Both run in both directions.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ctadl_ascent::facts::{GLOBAL_FRAME, GLOBAL_FRAME_NAME};
use serde::Deserialize;

use crate::exec;

/// A column of a canonical row: as stored, or a function or import id, joined to its name.
#[derive(Clone, Copy)]
enum Col {
    Plain(&'static str),
    Function(&'static str),
    Import(&'static str),
}

use Col::{Function, Import, Plain};

/// The tables checked, each with its canonical columns as stored in parquet.
const TABLES: &[(&str, &[Col])] = &[
    (
        "callee_resolvents",
        &[
            Plain("object_tag"),
            Function("object_func"),
            Plain("object_symbol"),
            Plain("context_tag"),
            Plain("context_name"),
            Plain("context_desc"),
            Function("target_id"),
        ],
    ),
    ("external_function", &[Function("func_id")]),
    (
        "formal_param",
        &[Function("func_id"), Plain("index"), Plain("type")],
    ),
    ("function_id", &[Plain("name")]),
    (
        "global_flow",
        &[
            Plain("dst"),
            Plain("dst_path"),
            Plain("src"),
            Plain("src_path"),
        ],
    ),
    ("globals", &[Plain("name"), Import("import_id")]),
    ("import_id", &[Plain("name")]),
    ("paths", &[Plain("path")]),
    (
        "summary",
        &[
            Function("func_id"),
            Plain("dst_index"),
            Plain("dst_global"),
            Plain("dst_path"),
            Plain("src_index"),
            Plain("src_global"),
            Plain("src_path"),
        ],
    ),
];

/// The tables the joins read names from.
const NAME_TABLES: [&str; 2] = ["function_id", "import_id"];

/// One table's counts, as `diff-index --json` and the query both name them.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
struct Counts {
    rows_a: u64,
    rows_b: u64,
    only_in_a: u64,
    only_in_b: u64,
    count_differs: u64,
}

#[derive(Deserialize)]
struct TableCounts {
    table: String,
    #[serde(flatten)]
    counts: Counts,
}

/// Compares `diff-index` with the query on each pair of consecutive indexes under `run`, the
/// scratch root of a regression run, and prints what disagrees. Returns whether everything
/// agreed. With fewer than two indexes, or no `duckdb` on `PATH`, it prints `SKIP` and the
/// reason, and returns true.
pub fn check(run: &Path, ctadl: &Path) -> Result<bool> {
    const NAME: &str = "diff-index vs SQL";
    let indexes = indexes(run)?;
    if indexes.len() < 2 {
        println!(
            "  SKIP  {NAME}  (fewer than two indexes under {})",
            run.display()
        );
        return Ok(true);
    }
    let Some(duckdb) = exec::which("duckdb") else {
        println!("  SKIP  {NAME}  (`duckdb` not found on PATH)");
        return Ok(true);
    };

    let (mut pairs, mut checked, mut agree) = (0, 0, true);
    for pair in indexes.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let has = |dir: &Path, t: &str| dir.join(format!("{t}.parquet")).is_file();
        if !NAME_TABLES.iter().all(|t| has(a, t) && has(b, t)) {
            println!(
                "  SKIP  {NAME}  ({} vs {}: no function or import names)",
                a.display(),
                b.display()
            );
            continue;
        }
        let tables: Vec<(&str, &[Col])> = TABLES
            .iter()
            .copied()
            .filter(|(t, _)| has(a, t) && has(b, t))
            .collect();
        pairs += 1;
        for (x, y) in [(a, b), (b, a)] {
            let ours = diff_index_counts(ctadl, x, y, &tables)?;
            let theirs = sql_counts(&duckdb, x, y, &tables)?;
            for (t, _) in &tables {
                checked += 1;
                if ours.get(*t) != theirs.get(*t) {
                    agree = false;
                    println!(
                        "  FAIL  {NAME}: {t}, A {}, B {}\n        diff-index: {:?}\n        duckdb:     {:?}",
                        x.display(),
                        y.display(),
                        ours.get(*t),
                        theirs.get(*t)
                    );
                }
            }
        }
    }
    println!(
        "Checked diff-index against SQL: {checked} table comparison(s) over {pairs} pair(s) of \
         {} indexes, both directions: {}",
        indexes.len(),
        if agree { "all agree" } else { "DISAGREE" }
    );
    Ok(agree)
}

/// Every index directory under `root` (one holding `index_config.json`), sorted by path.
fn indexes(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name() == Some("index_config.json".as_ref()) {
                out.push(dir.clone());
            }
        }
    }
    out.sort();
    Ok(out)
}

/// What `ctadl diff-index --json` reports for `tables` of `a` and `b`.
fn diff_index_counts(
    ctadl: &Path,
    a: &Path,
    b: &Path,
    tables: &[(&str, &[Col])],
) -> Result<BTreeMap<String, Counts>> {
    let mut cmd = Command::new(ctadl);
    cmd.args(["diff-index", "--json", "--limit", "0"]);
    for (t, _) in tables {
        cmd.args(["--table", t]);
    }
    cmd.arg(a).arg(b);
    let out = exec::run_with_timeout(cmd, "ctadl diff-index", Duration::from_secs(600))?;
    if !matches!(out.status.code(), Some(0 | 1)) {
        bail!(
            "ctadl diff-index {} {} failed ({}):\n{}",
            a.display(),
            b.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[derive(Deserialize)]
    struct Json {
        tables: Vec<TableCounts>,
    }
    let json: Json =
        serde_json::from_slice(&out.stdout).context("parsing `ctadl diff-index --json`")?;
    Ok(json
        .tables
        .into_iter()
        .map(|t| (t.table, t.counts))
        .collect())
}

/// What the query counts for `tables` of `a` and `b`.
fn sql_counts(
    duckdb: &Path,
    a: &Path,
    b: &Path,
    tables: &[(&str, &[Col])],
) -> Result<BTreeMap<String, Counts>> {
    let sql = counts_sql(a, b, tables);
    let mut cmd = Command::new(duckdb);
    cmd.args(["-json", "-c", &sql]);
    let out = exec::run_with_timeout(cmd, "duckdb", Duration::from_secs(600))?;
    if !out.status.success() {
        bail!(
            "duckdb failed ({}):\n{}\nquery:\n{sql}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let rows: Vec<TableCounts> =
        serde_json::from_slice(&out.stdout).context("parsing duckdb's JSON")?;
    Ok(rows.into_iter().map(|t| (t.table, t.counts)).collect())
}

/// The query: for each table, each side's canonical rows grouped and counted, the two joined
/// on every column (`IS NOT DISTINCT FROM`, so that NULL meets NULL), and the counts of the
/// join. One row per table.
fn counts_sql(a: &Path, b: &Path, tables: &[(&str, &[Col])]) -> String {
    let file = |dir: &Path, t: &str| {
        let path = dir.join(format!("{t}.parquet")).display().to_string();
        format!("read_parquet('{}')", path.replace('\'', "''"))
    };
    let mut q = String::from("WITH ");
    for (side, dir) in [("a", a), ("b", b)] {
        write!(
            q,
            "fn_{side} AS (SELECT id, name FROM {} UNION ALL SELECT {}, '{}'),\n\
             imp_{side} AS (SELECT id, name FROM {}),\n",
            file(dir, "function_id"),
            GLOBAL_FRAME.id,
            GLOBAL_FRAME_NAME,
            file(dir, "import_id"),
        )
        .unwrap();
        for (t, cols) in tables {
            let mut exprs = Vec::new();
            let mut joins = String::new();
            for (i, col) in cols.iter().enumerate() {
                let (expr, join) = match *col {
                    Plain(c) => (format!("t.\"{c}\""), None),
                    Function(c) => (format!("j{i}.name"), Some(("fn", c))),
                    Import(c) => (format!("j{i}.name"), Some(("imp", c))),
                };
                exprs.push(format!("{expr} AS c{i}"));
                if let Some((names, c)) = join {
                    write!(
                        joins,
                        " LEFT JOIN {names}_{side} j{i} ON j{i}.id = t.\"{c}\""
                    )
                    .unwrap();
                }
            }
            let cs: Vec<String> = (0..cols.len()).map(|i| format!("c{i}")).collect();
            writeln!(
                q,
                "{t}_{side} AS (SELECT {}, count(*) AS n FROM (SELECT {} FROM {} t{joins}) GROUP BY ALL),",
                cs.join(", "),
                exprs.join(", "),
                file(dir, t),
            )
            .unwrap();
        }
    }
    // The last CTE ends in ",\n"; the query starts after it.
    q.truncate(q.len() - 2);
    q.push('\n');
    let selects: Vec<String> = tables
        .iter()
        .map(|(t, cols)| {
            let on: Vec<String> = (0..cols.len())
                .map(|i| format!("a.c{i} IS NOT DISTINCT FROM b.c{i}"))
                .collect();
            format!(
                "SELECT '{t}' AS \"table\",\n  \
                 CAST(coalesce(sum(a.n), 0) AS BIGINT) AS rows_a,\n  \
                 CAST(coalesce(sum(b.n), 0) AS BIGINT) AS rows_b,\n  \
                 count(*) FILTER (WHERE b.n IS NULL) AS only_in_a,\n  \
                 count(*) FILTER (WHERE a.n IS NULL) AS only_in_b,\n  \
                 count(*) FILTER (WHERE a.n <> b.n) AS count_differs\n\
                 FROM {t}_a a FULL OUTER JOIN {t}_b b ON {}",
                on.join(" AND ")
            )
        })
        .collect();
    q.push_str(&selects.join("\nUNION ALL\n"));
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctadl_ascent::facts::{
        CallDispatchKey, CallDispatchKeyKind, CallTargetObject, CallTargetObjectKind, FormalIndex,
        FormalType, FunctionId, GlobalId, ImportId, Path as FactPath, Root, Str, schema,
    };
    use source_info::FileSpanId;

    /// Writes one side's `function_id` and `import_id` into `dir`, then calls `rows`, which
    /// writes the other tables, with the ids of each function and import by name.
    fn write_side(
        dir: &Path,
        functions: &[(u32, &str)],
        imports: &[(u32, &str)],
        rows: impl FnOnce(&dyn Fn(&str) -> FunctionId, &dyn Fn(&str) -> ImportId),
    ) {
        std::fs::create_dir_all(dir).unwrap();
        let fun = |name: &str| FunctionId::new(functions.iter().find(|f| f.1 == name).unwrap().0);
        let imp = |name: &str| ImportId(imports.iter().find(|i| i.1 == name).unwrap().0);
        schema::function_id::try_save(
            dir,
            functions
                .iter()
                .map(|&(id, name)| (FunctionId::new(id), Str::from(name).into())),
        )
        .unwrap();
        schema::import_id::try_save(
            dir,
            imports
                .iter()
                .map(|&(id, name)| (ImportId(id), name.to_string())),
        )
        .unwrap();
        rows(&fun, &imp);
    }

    /// Two sides whose ids differ and whose rows differ in known ways, so that each count is
    /// known. Both directions match the expected counts.
    #[test]
    fn sql_counts_match_known_tables() {
        let Some(duckdb) = exec::which("duckdb") else {
            eprintln!("skipped: `duckdb` not found on PATH");
            return;
        };
        let root =
            std::env::temp_dir().join(format!("ctadl_xtask_diff_sql_test_{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        let p = |s: &str| FactPath::parse(s).unwrap();
        let g = GlobalId::new("G");
        let c_key = || -> CallDispatchKey { CallDispatchKeyKind::C.into() };
        let ptr = |f| -> CallTargetObject { CallTargetObjectKind::FunctionId(f).into() };
        let summary = |global: bool| {
            let src = if global {
                Root::global(g)
            } else {
                Root::formal(FormalIndex::new(0))
            };
            (GLOBAL_FRAME, Root::global(g), p(".x"), src, p(""))
        };
        write_side(&a, &[(0, "f"), (1, "g")], &[(0, "x")], |fun, imp| {
            schema::paths::try_save(&a, [(p(".a"),), (p(".a"),), (p(".b"),)]).unwrap();
            schema::formal_param::try_save(
                &a,
                [
                    (fun("f"), FormalIndex::new(0), FormalType::Shared),
                    (fun("g"), FormalIndex::new(0), FormalType::Shared),
                ],
            )
            .unwrap();
            schema::summary::try_save(&a, [summary(false), summary(false), summary(true)]).unwrap();
            schema::callee_resolvents::try_save(&a, [(ptr(fun("f")), c_key(), fun("g"))]).unwrap();
            schema::globals::try_save(&a, [(g, imp("x"), Some(FileSpanId(1)))]).unwrap();
            schema::external_function::try_save(&a, [(fun("g"),)]).unwrap();
            schema::global_flow::try_save(&a, []).unwrap();
        });
        write_side(
            &b,
            &[(5, "g"), (7, "f"), (8, "h")],
            &[(3, "x"), (4, "y")],
            |fun, imp| {
                schema::paths::try_save(&b, [(p(".a"),), (p(".c"),)]).unwrap();
                schema::formal_param::try_save(
                    &b,
                    [
                        (fun("f"), FormalIndex::new(0), FormalType::Shared),
                        (fun("g"), FormalIndex::new(0), FormalType::Copied),
                    ],
                )
                .unwrap();
                schema::summary::try_save(&b, [summary(false), summary(true)]).unwrap();
                schema::callee_resolvents::try_save(&b, [(ptr(fun("f")), c_key(), fun("g"))])
                    .unwrap();
                schema::globals::try_save(&b, [(g, imp("x"), Some(FileSpanId(9)))]).unwrap();
                schema::external_function::try_save(&b, [(fun("g"),), (fun("g"),)]).unwrap();
                schema::global_flow::try_save(&b, []).unwrap();
            },
        );

        let c = |rows_a, rows_b, only_in_a, only_in_b, count_differs| Counts {
            rows_a,
            rows_b,
            only_in_a,
            only_in_b,
            count_differs,
        };
        let expected: BTreeMap<String, Counts> = [
            ("callee_resolvents", c(1, 1, 0, 0, 0)),
            ("external_function", c(1, 2, 0, 0, 1)),
            ("formal_param", c(2, 2, 1, 1, 0)),
            ("function_id", c(2, 3, 0, 1, 0)),
            ("global_flow", c(0, 0, 0, 0, 0)),
            ("globals", c(1, 1, 0, 0, 0)),
            ("import_id", c(1, 2, 0, 1, 0)),
            ("paths", c(3, 2, 1, 1, 1)),
            ("summary", c(3, 2, 0, 0, 1)),
        ]
        .into_iter()
        .map(|(t, c)| (t.to_string(), c))
        .collect();
        let swapped: BTreeMap<String, Counts> = expected
            .iter()
            .map(|(t, c)| {
                let s = Counts {
                    rows_a: c.rows_b,
                    rows_b: c.rows_a,
                    only_in_a: c.only_in_b,
                    only_in_b: c.only_in_a,
                    count_differs: c.count_differs,
                };
                (t.clone(), s)
            })
            .collect();

        let ab = sql_counts(&duckdb, &a, &b, TABLES);
        let ba = sql_counts(&duckdb, &b, &a, TABLES);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(ab.unwrap(), expected);
        assert_eq!(ba.unwrap(), swapped);
    }
}
