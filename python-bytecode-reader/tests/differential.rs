//! Differential round-trip test (the strongest reader test): run the serializer
//! twice — once as stable text, once as the JSON oracle — parse the stable text
//! with the reader, deserialize the JSON with serde, and assert the two
//! `BytecodeFile`s are equal.
//!
//! Both outputs derive from the *same* normalized records inside the serializer,
//! so any mismatch is a reader (grammar/extraction) bug.
//!
//! Self-skips (prints a note, passes) when no Python interpreter is available, so
//! `cargo test` stays green on a machine without one. Set `PYTHON` (or have
//! `python3` on PATH) to exercise it; repeat with `PYTHON=python3.11 …` for
//! multi-version coverage.

use std::io::Write;
use std::path::Path;

use python_bytecode_reader::serialize::{Format, run_serializer};
use python_bytecode_reader::{BytecodeFile, SerializeError, parse};

/// True if we have no interpreter — used to self-skip rather than fail.
fn interpreter_missing(err: &SerializeError) -> bool {
    matches!(err, SerializeError::NoInterpreter)
}

/// Serialize `source` twice and assert the reader's parse of the stable text
/// equals the JSON oracle. Returns `false` if Python is unavailable (skip).
fn check_source(source: &Path) -> bool {
    let stable = match run_serializer(source, Format::Stable) {
        Ok(s) => s,
        Err(e) if interpreter_missing(&e) => return false,
        Err(e) => panic!("stable serialize failed: {e}"),
    };
    let json = match run_serializer(source, Format::Json) {
        Ok(s) => s,
        Err(e) if interpreter_missing(&e) => return false,
        Err(e) => panic!("json serialize failed: {e}"),
    };

    let parsed: BytecodeFile = parse(&stable).expect("reader parses stable text");
    let oracle: BytecodeFile = serde_json::from_str(&json).expect("deserialize json oracle");

    assert_eq!(
        parsed,
        oracle,
        "reader disagreed with the JSON oracle for {}",
        source.display()
    );

    // Structural sanity: at least one code object, and every instruction has an
    // opname (so a silently-empty parse can't pass).
    assert!(!parsed.code_objects.is_empty());
    for co in &parsed.code_objects {
        for insn in &co.instructions {
            assert!(!insn.opname.is_empty());
        }
    }
    true
}

/// A collection of Python snippets exercising the format's corners.
const SNIPPETS: &[(&str, &str)] = &[
    ("straight_line", "def f(a):\n    b = a\n    return b\n"),
    (
        "branch_and_call",
        "def g(x):\n    if x:\n        y = h(x)\n    else:\n        y = 0\n    return y\n\ndef h(z):\n    return z\n",
    ),
    (
        "closures_and_comprehension",
        "def outer(items):\n    total = 0\n    def inner(v):\n        return v + total\n    return [inner(i) for i in items]\n",
    ),
    (
        "strings_and_unicode",
        "def s():\n    a = \"caf\\u00e9\"\n    b = \"tab\\there\"\n    c = \"emoji \\U0001F600\"\n    return (a, b, c)\n",
    ),
    (
        "attributes_and_subscript",
        "def m(o, k):\n    o.field = o.other\n    return o[k]\n",
    ),
    (
        "loops",
        "def loop(n):\n    acc = 0\n    for i in range(n):\n        acc = acc + i\n    while acc > 0:\n        acc = acc - 1\n    return acc\n",
    ),
];

#[test]
fn differential_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut skipped = false;
    for (name, code) in SNIPPETS {
        let path = dir.path().join(format!("{name}.py"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(code.as_bytes()).unwrap();
        drop(f);
        if !check_source(&path) {
            skipped = true;
            break;
        }
    }
    if skipped {
        eprintln!(
            "SKIP differential_round_trip: no Python interpreter (set PYTHON or add python3 to PATH)"
        );
    }
}

/// Locate the interpreter the serializer would use for source (`PYTHON`, else
/// `python3`), or `None` to self-skip when no interpreter is present.
fn default_interpreter() -> Option<std::path::PathBuf> {
    match std::env::var_os("PYTHON") {
        Some(p) => Some(std::path::PathBuf::from(p)),
        None => which::which("python3").ok(),
    }
}

/// Compile `source` under `python` into `cfile`, a real `.pyc` carrying that
/// interpreter's bytecode magic.
fn compile_pyc(python: &Path, source: &Path, cfile: &Path) {
    let status = std::process::Command::new(python)
        .arg("-c")
        .arg(format!(
            "import py_compile; py_compile.compile(r'{}', cfile=r'{}', doraise=True)",
            source.display(),
            cfile.display()
        ))
        .status()
        .expect("run py_compile");
    assert!(status.success(), "py_compile failed");
}

/// The `.pyc` path: compile a source to a real `.pyc` and check it round-trips
/// the same way source does — this exercises the version-matching dispatch
/// ([`find_matching_interpreter`]) on its happy path.
#[test]
fn differential_pyc() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("mod.py");
    std::fs::write(&src, b"def f(a):\n    b = a\n    return b\n").unwrap();

    let Some(python) = default_interpreter() else {
        eprintln!("SKIP differential_pyc: no Python interpreter");
        return;
    };
    let pyc = dir.path().join("mod.pyc");
    compile_pyc(&python, &src, &pyc);
    check_source(&pyc);
}

/// A `.pyc` whose bytecode magic matches no available interpreter must be
/// *refused* — never handed to `marshal`, which could crash or return garbage.
/// We corrupt the magic of a real `.pyc` and assert a clean, actionable error.
#[test]
fn pyc_magic_mismatch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("mod.py");
    std::fs::write(&src, b"def f(a):\n    return a\n").unwrap();

    let Some(python) = default_interpreter() else {
        eprintln!("SKIP pyc_magic_mismatch_is_refused: no Python interpreter");
        return;
    };
    let pyc = dir.path().join("mod.pyc");
    compile_pyc(&python, &src, &pyc);

    // Flip a magic byte so it matches no real interpreter's bytecode version.
    let mut data = std::fs::read(&pyc).unwrap();
    data[0] ^= 0xFF;
    std::fs::write(&pyc, &data).unwrap();

    let err = run_serializer(&pyc, Format::Stable)
        .expect_err("mismatched-magic .pyc must be rejected, not marshalled");
    match err {
        SerializeError::NoMatchingInterpreter { magic, tried, .. } => {
            // The reported magic is the corrupted file's, and we recorded the
            // interpreters we probed (at least the default one).
            assert_eq!(magic.len(), 8, "magic is 4 bytes as hex: {magic}");
            assert!(!tried.is_empty(), "should list the interpreters tried");
        }
        other => panic!("expected NoMatchingInterpreter, got: {other}"),
    }
}

/// A too-short `.pyc` (fewer than the 4 magic bytes) yields a located `Pyc`
/// error rather than a panic or an out-of-bounds read. Needs no interpreter.
#[test]
fn truncated_pyc_is_a_clean_error() {
    let dir = tempfile::tempdir().unwrap();
    let pyc = dir.path().join("short.pyc");
    std::fs::write(&pyc, b"\xf3\x0d").unwrap(); // 2 bytes: shorter than the magic

    let err = run_serializer(&pyc, Format::Stable).expect_err("truncated .pyc must error");
    match err {
        SerializeError::Pyc { path, .. } => assert!(path.ends_with("short.pyc")),
        other => panic!("expected Pyc error, got: {other}"),
    }
}

/// Backstop: even if dispatch is bypassed and a mismatched interpreter is forced
/// onto a `.pyc` directly, the Python `_load_code` guard refuses to `marshal` it
/// (exiting non-zero with a magic-mismatch message) instead of risking a crash.
#[test]
fn python_guard_refuses_mismatched_magic() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("mod.py");
    std::fs::write(&src, b"def f(a):\n    return a\n").unwrap();

    let Some(python) = default_interpreter() else {
        eprintln!("SKIP python_guard_refuses_mismatched_magic: no Python interpreter");
        return;
    };
    let pyc = dir.path().join("mod.pyc");
    compile_pyc(&python, &src, &pyc);
    let mut data = std::fs::read(&pyc).unwrap();
    data[0] ^= 0xFF; // corrupt magic -> won't match this interpreter
    std::fs::write(&pyc, &data).unwrap();

    // Invoke the module directly (bypassing Rust dispatch) against the very
    // interpreter that just compiled it — the guard must still refuse.
    let pkg_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("python");
    let output = std::process::Command::new(&python)
        .env("PYTHONPATH", &pkg_root)
        .args(["-m", "bytecode_text"])
        .arg(&pyc)
        .output()
        .expect("run bytecode_text module");
    assert!(
        !output.status.success(),
        "guard should reject mismatched .pyc"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not match this interpreter"),
        "guard message missing; stderr was: {stderr}"
    );
}
