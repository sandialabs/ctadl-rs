//! The CIL taint cases: `nightly/tests/cil/<name>.cs`, or a directory `<name>/` of `.cs` files
//! that are separate assemblies, each paired with `<name>-query.json5`, its config.
//!
//! A case is compiled with `dotnet build` (Release, like the exec fixtures, with a portable PDB
//! beside each assembly), imported with `ctadl import -l cil` (the output directory, for a
//! multi-assembly case), indexed and queried. The query model is generated from the config:
//! `Source()` and `SourceInt()` return a source, and each function the config names among
//! `expected_sinks` and `unexpected_sinks` sinks its argument 0. A case may add rules of its own
//! in a Portcullis file beside the config, `<name>-query.ctadl`, which the query loads too.
//!
//! A case is judged twice. By the sinks its code flows end in: every simple name in
//! `expected_sinks` must be the sink of a code flow that starts at a source, and no name in
//! `unexpected_sinks` may be; giving each scenario of a file its own sink function keeps the
//! scenarios apart. And by source lines, as a Lua case is: every line in `expected_lines` must
//! be a code-flow step's `startLine`, and none in `unexpected_lines` may be. The PDB gives the
//! steps their lines; in a multi-assembly case the lines of all its files are one set.
//!
//! In a multi-assembly case the files are built in name order, each referencing the ones
//! built before it, and the assembly is named after the file stem.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use ctadl_ascent::models::portcullis::migrate::quote;
use serde_json::Value;

use crate::exec;
use crate::regression::Outcome;

/// What a case config says.
pub struct Config {
    pub expected_sinks: Vec<String>,
    pub unexpected_sinks: Vec<String>,
    pub expected_lines: Vec<i64>,
    pub unexpected_lines: Vec<i64>,
}

pub fn read_config(path: &Path) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value: Value =
        json5::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let names = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    Ok(Config {
        expected_sinks: names("expected_sinks"),
        unexpected_sinks: names("unexpected_sinks"),
        expected_lines: crate::assertions::read_expected_lines(path)?,
        unexpected_lines: crate::assertions::read_unexpected_lines(path)?,
    })
}

/// The query model for a case, in Portcullis.
pub fn query_model(config: &Config) -> String {
    let mut model = String::from(
        "source(F::return, kind = \"UserInput\") :-\n  fun(F, name in {\"Source\", \"SourceInt\"});\n",
    );
    let sinks: Vec<String> = config
        .expected_sinks
        .iter()
        .chain(&config.unexpected_sinks)
        .map(|name| quote(name))
        .collect();
    // With no sinks to name there is no sink rule: `name in {}` would be a rule that can never
    // match, which the model check warns about.
    if !sinks.is_empty() {
        model.push_str(&format!(
            "\nsink(F::arg(0), kind = \"Output\") :-\n  fun(F, name in {{{}}});\n",
            sinks.join(", ")
        ));
    }
    model
}

/// The `dotnet` to build with, if there is one.
pub fn dotnet() -> Option<PathBuf> {
    exec::which("dotnet")
}

/// Builds the case into `out` and returns what to import: the assembly, or `out` itself for
/// a multi-assembly case.
pub fn compile(dotnet: &Path, source: &Path, work: &Path, out: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let files: Vec<PathBuf> = if source.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(source)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cs"))
            .collect();
        files.sort();
        files
    } else {
        vec![source.to_path_buf()]
    };
    if files.is_empty() {
        bail!("no .cs files in {}", source.display());
    }
    let mut built: Vec<PathBuf> = Vec::new();
    for cs in &files {
        let stem = cs
            .file_stem()
            .and_then(|s| s.to_str())
            .context("a source with no name")?
            .to_string();
        let project = work.join("build").join(&stem);
        std::fs::create_dir_all(&project)?;
        let references: String = built
            .iter()
            .map(|dll| {
                let name = dll.file_stem().unwrap().to_string_lossy();
                format!(
                    "    <Reference Include=\"{name}\"><HintPath>{}</HintPath></Reference>\n",
                    dll.display()
                )
            })
            .collect();
        let csproj = format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <AssemblyName>{stem}</AssemblyName>
    <Optimize>true</Optimize>
    <Deterministic>true</Deterministic>
    <DebugType>portable</DebugType>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
    <GenerateAssemblyInfo>false</GenerateAssemblyInfo>
    <Nullable>disable</Nullable>
    <LangVersion>latest</LangVersion>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <AppendTargetFrameworkToOutputPath>false</AppendTargetFrameworkToOutputPath>
  </PropertyGroup>
  <ItemGroup>
    <Compile Include="{}" />
{references}  </ItemGroup>
</Project>
"#,
            cs.display()
        );
        std::fs::write(project.join(format!("{stem}.csproj")), csproj)?;
        let bin = project.join("bin");
        let mut cmd = Command::new(dotnet);
        cmd.current_dir(&project)
            .args(["build", "-c", "Release", "--nologo", "-v", "q", "-o"])
            .arg(&bin)
            .env("DOTNET_NOLOGO", "1")
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1");
        exec::run_checked(cmd, "dotnet build")?;
        for ext in ["dll", "pdb"] {
            let file = bin.join(format!("{stem}.{ext}"));
            std::fs::copy(&file, out.join(format!("{stem}.{ext}")))
                .with_context(|| format!("copying {}", file.display()))?;
        }
        built.push(out.join(format!("{stem}.dll")));
    }
    Ok(if source.is_dir() {
        out.to_path_buf()
    } else {
        built.remove(0)
    })
}

/// Judges a case's SARIF against its config.
pub fn check(config: &Config, sarif: &Path) -> Result<Outcome> {
    let functions = crate::assertions::collect_codeflow_sink_functions(sarif)?;
    let reached: BTreeSet<String> = functions.iter().map(|f| simple_name(f)).collect();
    let missing: Vec<&String> = config
        .expected_sinks
        .iter()
        .filter(|s| !reached.contains(*s))
        .collect();
    let unexpected: Vec<&String> = config
        .unexpected_sinks
        .iter()
        .filter(|s| reached.contains(*s))
        .collect();
    let mut why = Vec::new();
    if !missing.is_empty() {
        why.push(format!(
            "expected sinks not reached: {missing:?} (reached {reached:?})"
        ));
    }
    if !unexpected.is_empty() {
        why.push(format!(
            "unexpected sinks reached: {unexpected:?} (reached {reached:?})"
        ));
    }

    let lines = crate::assertions::collect_codeflow_start_lines(sarif)?;
    let missing_lines: Vec<i64> = config
        .expected_lines
        .iter()
        .copied()
        .filter(|line| !lines.contains(line))
        .collect();
    if !missing_lines.is_empty() {
        why.push(format!(
            "expected lines {missing_lines:?} not reached (reached {lines:?})"
        ));
    }
    if let Some(bad) = crate::assertions::check_unexpected_lines(&config.unexpected_lines, &lines) {
        why.push(bad);
    }

    Ok(if why.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(why.join("; "))
    })
}

/// `Sink1` in `[case]Cases.Taint::Sink1(System.Object)->System.Void`.
fn simple_name(fid: &str) -> String {
    let after = fid.rsplit_once("::").map(|(_, m)| m).unwrap_or(fid);
    after.split('(').next().unwrap_or(after).to_string()
}

#[cfg(test)]
mod tests {
    use ctadl_ascent::models::portcullis::parse::parse_program;

    use super::{Config, query_model};

    fn config(expected: &[&str], unexpected: &[&str]) -> Config {
        Config {
            expected_sinks: expected.iter().map(|s| s.to_string()).collect(),
            unexpected_sinks: unexpected.iter().map(|s| s.to_string()).collect(),
            expected_lines: Vec::new(),
            unexpected_lines: Vec::new(),
        }
    }

    /// The generated model is what `ctadl migrate-models --language cil` writes for the JSON
    /// generators it replaced, and it parses.
    #[test]
    fn the_query_model_is_portcullis() {
        let model = query_model(&config(&["Sink1", "Sink2"], &["Clean1"]));
        assert_eq!(
            model,
            "source(F::return, kind = \"UserInput\") :-\n  \
             fun(F, name in {\"Source\", \"SourceInt\"});\n\
             \n\
             sink(F::arg(0), kind = \"Output\") :-\n  \
             fun(F, name in {\"Sink1\", \"Sink2\", \"Clean1\"});\n"
        );
        let program = parse_program(&model).expect("the query model parses");
        assert_eq!(program.rules.len(), 2);

        let sinkless = query_model(&config(&[], &[]));
        let program = parse_program(&sinkless).expect("the query model parses");
        assert_eq!(program.rules.len(), 1, "no sinks named, so no sink rule");
    }
}
