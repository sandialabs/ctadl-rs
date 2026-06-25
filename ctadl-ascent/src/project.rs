/*! Access to imported artifacts and analysis projects

# CTADL store

CTADL state is stored into `XDG_STATE_HOME/ctadl`. This can be overriden on the command line by setting the `XDG_STATE_HOME` variable (or calling [`init_store_path`]). We call this directory the store. There are two important store paths:

- `imports`: Where individual artifacts are stored. Each subdirectory corresponds to an artifact that was imported into CTADL. Each import is a directory and contains an `import_config.json` that stores, at least, the original path to the thing imported.

- `projects`: Where analysis projects are stored. When you decide to index some code, you have to supply a project name, which is created as a subdirectory. Each project has a `project_config.json` that records the artifacts that went into the project and any analysis parameters that were used for indexing.

On import, the artifact is parsed and translated into a `ctadl_ir::Program`, then encoded as a binary blob and put into the relevant `imports` subdirectory. Then we write the `import_config.json` to the same directory.

A project represents a set of programs that have been indexed together. It might be a single apk, or it might be a java jar and C code that implements JNI calls from java. Inside a project the layout is:

- `project_config.json`: Configuration file. Has the name and original artifacts of the project.
- `index`: Stores parquet files, the output of indexing.
- `query`: Stores parquet files, the output of a taint analysis query.


*/

use std::env;
use std::fs::{File, canonicalize};
use std::sync::OnceLock;
use std::{
    path,
    path::{Component, Path, PathBuf},
};

use hashbrown::hash_set::HashSet;

use crate::error::{Error, ErrorContext};

/// Store path. Defaults to `XDG_STATE_HOME`.
///
/// This can be customized through `init_store_path`, but it can only be initialized once.
static STORE_PATH: OnceLock<PathBuf> = OnceLock::new();

#[inline]
fn default_store_path() -> PathBuf {
    get_xdg_state_home().join("ctadl")
}

/// Initializes the store path for this process. If you don't call this function, CTADL uses
/// default store paths (see [`StorePaths`]). If you need to override CTADL's store path from a
/// library, you should call this function before doing anything else with the library. If called
/// again with a different value, returns Err.
pub fn init_store_path<P: AsRef<Path>>(override_path: Option<P>) -> Result<(), &'static str> {
    let value = override_path
        .map(|p| p.as_ref().to_path_buf())
        .unwrap_or_else(default_store_path);

    STORE_PATH
        .set(value)
        .map_err(|_| "STORE_PATH already initialized")
}

/// Represents our local import of an artifact
#[derive(Clone, serde::Serialize, serde::Deserialize, Debug)]
pub struct ArtifactImport {
    /// Name of the import for 'index' to reference
    pub name: String,
    pub language: ArtifactLanguage,
    /// Path to the original artifact
    pub artifact_path: PathBuf,
    /// Path to the import directory for the artifact.
    pub import_path: PathBuf,
    pub version: String,
    /// Base address the disassembler (Ghidra) loaded the artifact at. Used to
    /// recover section-relative offsets from absolute instruction addresses
    /// (e.g. for `addr2line`). `None` for non-binary imports or older imports
    /// created before this field existed.
    #[serde(default)]
    pub image_base: Option<i64>,
    /// Hex-encoded SHA-256 content hash of the artifact, recorded once the import
    /// has successfully completed. Together with [`Self::artifact_path`] this lets
    /// `import --skip-existing` decide whether a re-import is necessary. `None` for
    /// older imports created before this field existed, or for an import that has
    /// not yet completed.
    #[serde(default)]
    pub hash: Option<String>,
}

impl ArtifactImport {
    /// Creates a new import in store. The config is created and saved at this point.
    ///
    /// An import is a place to store an IR program, plus whatever metadata, about artifacts.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be canonicalized or if there is an error creating
    /// config file.
    pub fn try_create(
        name: &str,
        language: ArtifactLanguage,
        artifact_path: &Path,
    ) -> Result<Self, Error> {
        let artifact_path = canonicalize(artifact_path)?;
        let import_path = StorePaths::import_path().join(name);
        std::fs::create_dir_all(&import_path)?;
        let result = Self {
            name: name.to_owned(),
            language,
            artifact_path,
            import_path,
            version: "1".to_string(),
            image_base: None,
            hash: None,
        };
        result.save()?;
        Ok(result)
    }

    /// Writes config to the config path
    ///
    /// # Errors
    ///
    /// If there are i/o or deserialization errors
    pub fn save(&self) -> Result<(), Error> {
        let path = self.config_path();
        let file = File::create(&path)?;
        serde_json::to_writer(file, &self)?;
        log::info!(
            "wrote import configuration to '{}'",
            path::absolute(&path)?.display()
        );
        Ok(())
    }

    /// Loads config from path
    ///
    /// # Errors
    ///
    /// If there are i/o or deserialization errors
    #[inline]
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let file = File::open(path)?;
        let result = serde_json::from_reader(file)?;
        Ok(result)
    }

    /// Loads config using the store
    ///
    /// # Errors
    ///
    /// If there are i/o or deserialization errors
    pub fn load_by_name(name: &str) -> Result<Self, Error> {
        let path = StorePaths::import_path()
            .join(name)
            .join("import_config.json");
        Self::load(&path).err_context(|| format!("reading import config: '{}'", path.display()))
    }

    /// Path to the serialized IR program for this artifact
    #[inline]
    pub fn program_path(&self) -> PathBuf {
        self.import_path.join("ir-program.bitcode")
    }

    /// Path to the serialized virtual method table
    pub fn vmt_path(&self) -> PathBuf {
        self.import_path.join("ir-vmt.bitcode")
    }

    /// Path to the serialized flowy requirements
    pub fn requirements_path(&self) -> PathBuf {
        self.import_path.join("tnt-requirements.bitcode")
    }

    pub fn source_info_dir(&self) -> PathBuf {
        self.import_path.join("source-info")
    }

    /// Path to the IR program for this artifact
    #[inline]
    pub fn config_path(&self) -> PathBuf {
        self.import_path.join("import_config.json")
    }

    /// True if the import destination (the serialized IR program) is already present
    /// in the store. Used together with [`Self::hash`] to decide whether an import can
    /// be skipped.
    #[inline]
    pub fn destination_exists(&self) -> bool {
        self.program_path().exists()
    }

    /// Records the artifact's content hash in the config and persists it. Call this
    /// once an import has completed successfully so that a later `--skip-existing`
    /// import can detect that the stored artifact is up to date.
    ///
    /// # Errors
    ///
    /// If the artifact cannot be read or the config cannot be written.
    pub fn record_artifact_hash(&mut self) -> Result<(), Error> {
        self.hash = Some(hash_artifact(&self.artifact_path)?);
        self.save()
    }

    /// Returns true if an import named `name` already exists in the store, its
    /// destination is present, and both the stored artifact path and content hash
    /// match `artifact_path` and its current contents. When this holds, a re-import
    /// would reproduce the same result and can be skipped.
    ///
    /// Returns `false` (rather than erroring) when no matching import config can be
    /// loaded, so the caller falls back to performing the import.
    ///
    /// # Errors
    ///
    /// If the artifact path cannot be canonicalized or its contents cannot be hashed.
    pub fn is_up_to_date(name: &str, artifact_path: &Path) -> Result<bool, Error> {
        let config = match Self::load_by_name(name) {
            Ok(config) => config,
            // No (readable) prior import: not up to date, so the caller imports.
            Err(_) => return Ok(false),
        };
        if !config.destination_exists() {
            return Ok(false);
        }
        let stored_hash = match &config.hash {
            Some(hash) => hash,
            None => return Ok(false),
        };
        // Compare canonicalized paths so the stored path (always canonical) lines up.
        let artifact_path = canonicalize(artifact_path)?;
        if config.artifact_path != artifact_path {
            return Ok(false);
        }
        Ok(*stored_hash == hash_artifact(&artifact_path)?)
    }
}

/// Computes a stable, hex-encoded SHA-256 content hash of an artifact, which may be a
/// single file or a directory (e.g. a directory of C sources or Ghidra pcode facts).
///
/// For a directory the hash covers every regular file underneath it, incorporating
/// each file's path relative to the directory and its contents, in a deterministic
/// (sorted) order so the result is independent of filesystem iteration order.
///
/// # Errors
///
/// If the artifact (or any file within it) cannot be read.
pub fn hash_artifact(path: &Path) -> Result<String, Error> {
    use source_info::ContentHasher;

    let mut hasher = ContentHasher::new();
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        let mut files = Vec::new();
        collect_files(path, &mut files)?;
        files.sort();
        for file in &files {
            let rel = file.strip_prefix(path).unwrap_or(file);
            let rel_bytes = rel.to_string_lossy();
            // Length-prefix path and contents so distinct trees can't collide by
            // concatenation.
            hasher.update(&(rel_bytes.len() as u64).to_le_bytes());
            hasher.update(rel_bytes.as_bytes());
            let data = std::fs::read(file)
                .map_err(Error::Io)
                .err_context(|| format!("hashing artifact file: {}", file.display()))?;
            hasher.update(&(data.len() as u64).to_le_bytes());
            hasher.update(&data);
        }
    } else {
        let data = std::fs::read(path)
            .map_err(Error::Io)
            .err_context(|| format!("hashing artifact: {}", path.display()))?;
        hasher.update(&data);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Recursively collects regular files under `dir` into `out`.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_files(&path, out)?;
        } else if metadata.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Hex-encodes a byte slice using lowercase digits.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// An analysis project allows you to index single or multiple artifacts together.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct AnalysisProject {
    pub name: String,
    /// Project directory in the store
    pub dir: PathBuf,
    /// Names of the imports referred to by this project
    pub imports: Vec<String>,
}

impl AnalysisProject {
    /// Creates analysis project in the store under `name`. The `import_names` must refer to
    /// previously imported artifacts. See [`crate::cli::import`].
    ///
    /// # Errors
    ///
    /// If project path cannot be canonicalized, created, or there is an error creating the config
    pub fn try_create<S: AsRef<str>>(
        name: &str,
        import_names: &[S],
    ) -> Result<AnalysisProject, Error> {
        let path = StorePaths::projects_path().join(name);
        std::fs::create_dir_all(&path)
            .map_err(Error::Io)
            .err_context(|| format!("in create project dir: {}", path.display()))?;
        let dir = canonicalize(&path)
            .map_err(Error::Io)
            .err_context(|| format!("in canonicalize project dir: {}", path.display()))?;
        // Dedup import names (order-preserving): `index` co-indexes every argument, so a
        // repeated program name (e.g. `index amuled amuled`) would codegen its facts twice and
        // inflate every relation. Indexing the same import twice is never meaningful.
        let mut seen = std::collections::HashSet::new();
        let imports: Vec<String> = import_names
            .iter()
            .map(|s| s.as_ref().to_owned())
            .filter(|n| seen.insert(n.clone()))
            .collect();
        let result = Self {
            name: name.to_owned(),
            dir,
            imports,
        };
        result.save()?;
        Ok(result)
    }

    /// Load the analysis project from a path
    ///
    /// # Errors
    ///
    /// If there is an error reading or deserializing the configuration
    #[inline]
    pub fn try_load_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let result = serde_json::from_reader(file)
            .err_context(|| format!("deserializing config: '{}'", path.display()))?;
        Ok(result)
    }

    /// Load the analysis project by name from the store
    ///
    /// # Errors
    ///
    /// If there is an error reading or deserializing the configuration
    #[inline]
    pub fn try_load_name(name: &str) -> Result<Self, Error> {
        let path = StorePaths::projects_path()
            .join(name)
            .join("project_config.json");
        Self::try_load_path(&path).err_context(|| format!("loading config: '{}'", path.display()))
    }

    /// Loads artifact imports. Each item in the iterator may throw an error; see
    /// [`ArtifactImport::load`] for what those errors are.
    #[inline]
    pub fn iter_imports(&self) -> impl Iterator<Item = Result<ArtifactImport, Error>> {
        self.imports
            .iter()
            .map(|name| ArtifactImport::load_by_name(name.as_ref()))
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("project_config.json")
    }

    /// The path to the folder where the result of 'index' should be stored. Ensures the path is
    /// created.
    ///
    /// # Errors
    ///
    /// If there is an error creating the path
    #[inline]
    pub fn index_path(&self) -> Result<PathBuf, Error> {
        let path = self.dir.join("index");
        std::fs::create_dir_all(&path)
            .map_err(Error::Io)
            .err_context(|| format!("in create index dir: '{}'", path.display()))?;
        Ok(path)
    }

    /// Save the analysis project configuration
    ///
    /// # Errors
    ///
    /// If there is an error serializing or writing the configuration
    #[inline]
    pub fn save(&self) -> Result<(), Error> {
        let path = self.config_path();
        let file = File::create(&path)?;
        serde_json::to_writer(file, &self)?;
        log::info!(
            "wrote project configuration to '{}'",
            path::absolute(&path)?.display()
        );
        Ok(())
    }
}

/// Encodes the store paths we use for things
pub struct StorePaths {}

impl StorePaths {
    /// Root of the store. By default, this is the "ctadl" directory in `XDG_STATE_HOME`. That
    /// behavior can be customized by calling [`init_store_path`] BEFORE any store interaction.
    #[inline]
    pub fn root() -> &'static Path {
        STORE_PATH.get_or_init(default_store_path).as_path()
    }

    /// Artifacts are imported to the "imports" subdirectory of the root
    #[inline]
    pub fn import_path() -> PathBuf {
        Self::root().join("imports")
    }

    /// Analysis projects are stored in the "imports" subdirectory of the root
    #[inline]
    pub fn projects_path() -> PathBuf {
        Self::root().join("projects")
    }
}

/// Returns the last path component, the artifact name, of a path. If there is no such component,
/// errors.
pub fn artifact_name<'a>(artifact: &'a Path) -> Result<Component<'a>, Error> {
    artifact.components().next_back().ok_or(Error::Path {
        message: "no last path component".to_string(),
    })
}

#[allow(dead_code)]
pub(crate) fn get_xdg_config_home() -> PathBuf {
    env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let home = env::var("HOME").ok()?;
            Some(PathBuf::from(home).join(".config"))
        })
        .unwrap()
}

#[allow(dead_code)]
pub(crate) fn get_xdg_data_home() -> PathBuf {
    env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let home = env::var("HOME").ok()?;
            Some(PathBuf::from(home).join(".local").join("share"))
        })
        .unwrap()
}

pub(crate) fn get_xdg_state_home() -> PathBuf {
    env::var("XDG_STATE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let home = env::var("HOME").ok()?;
            Some(PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap()
}

#[allow(dead_code)]
pub(crate) fn get_xdg_cache_home() -> PathBuf {
    env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let home = env::var("HOME").ok()?;
            Some(PathBuf::from(home).join(".cache"))
        })
        .unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArtifactLanguage {
    /// Treat as JVM bytecode inputs (e.g., .class)
    Jvm,
    /// Treat as JVM bytecode JAR inputs
    Jar,
    /// Treat as Android DEX inputs (e.g., .dex)
    Dex,
    /// Treat as Android APK inputs
    Apk,
    /// Treat as C files
    C,
    /// Treat as Ghidra pcode facts directory
    Pcode,
    /// Treat as Flowy file
    Flowy,
}

// XDG_RUNTIME_DIR, if it doesn't exist, requires creating something temporary, and I'd like that
// to be dropped on program exit, so I just didn't implement it yet.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DetectLanguage {
    Jadx,
}

#[derive(Debug, Default)]
pub struct LanguageSet {
    mems: HashSet<DetectLanguage>,
}

impl LanguageSet {
    pub fn insert(&mut self, lang: DetectLanguage) {
        self.mems.insert(lang);
    }

    pub fn contains(&self, lang: DetectLanguage) -> bool {
        self.mems.contains(&lang)
    }
}

impl std::iter::FromIterator<DetectLanguage> for LanguageSet {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = DetectLanguage>,
    {
        Self {
            mems: iter.into_iter().collect(),
        }
    }
}
