//! `ctadl diff-index`: finds the two index directories, refuses an index this build cannot
//! read, and compares them with [`ctadl_index_diff`].

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use ctadl_index_diff::{Options, Report, diff_index};

use crate::project::{AnalysisProject, INDEX_CONFIG_FILE, INDEX_FORMAT_VERSION, IndexConfig};

/// Compares the indexes `a` and `b` names, each an index directory or a project in the store.
///
/// # Errors
///
/// If either names neither a directory nor a project, if either index is not in the format
/// this build reads, or if a table cannot be read or made canonical.
pub fn diff_indexes(a: &str, b: &str, opts: &Options) -> anyhow::Result<Report> {
    let a = resolve_index(a)?;
    let b = resolve_index(b)?;
    check_index_format(&a)?;
    check_index_format(&b)?;
    Ok(diff_index(&a, &b, opts)?)
}

/// The index directory `arg` names: `arg` itself if it is a directory, otherwise the `index/`
/// directory of the project named `arg`. Nothing is created.
pub fn resolve_index(arg: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(arg);
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }
    let project = AnalysisProject::try_load_name(arg)
        .with_context(|| format!("{arg:?} is neither a directory nor a project in the store"))?;
    Ok(project.dir().join("index"))
}

/// Refuses an index whose `index_config.json` is missing or names a format other than the one
/// this build reads. The table decoders read only the current format.
pub fn check_index_format(dir: &Path) -> anyhow::Result<()> {
    let path = dir.join(INDEX_CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "{} has no {INDEX_CONFIG_FILE}: it is not an index, or `ctadl index` did not finish \
             writing it",
            dir.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let config: IndexConfig =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if config.version != INDEX_FORMAT_VERSION {
        bail!(
            "{} is in index format {}, and this build reads only format {INDEX_FORMAT_VERSION}; \
             re-run `ctadl index` to compare it",
            dir.display(),
            config.version
        );
    }
    Ok(())
}
