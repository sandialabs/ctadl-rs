//! Phase 2 — programmatic test-case generator.
//!
//! Grammar synthesis: build a program by threading a tainted value from
//! `source()` through a random sequence of "transform" snippets and into
//! `sink(...)`. Each transform takes the current carrier variable and produces a
//! new one (taint preserved, or killed). The generator does **not** compute the
//! oracle — DFSan determines the real flow when the harness scans the output.
//!
//! All generated programs are: compilable (plain clang and `-fsanitize=dataflow`),
//! UB-free (every variable written before read; in-bounds array indices; no null),
//! libc-free in the taint path, and deterministic (constant conditions, fixed
//! indices/loop counts).
//!
//! The transform vocabulary deliberately spans the constructs the tree-sitter C
//! frontend recently learned to ingest — arrays, `switch`, `goto`, function
//! pointers — and, more importantly, their **combinations** (e.g. an array of
//! function pointers, a pointer to a function pointer). Those combinations are the
//! untested surface where the next soundness gap is most likely to hide; the
//! generator's job is to thread real taint through them at volume and let DFSan
//! adjudicate while CTADL is compared against it.

use std::path::Path;

use anyhow::{Context, Result};

/// Deterministic, dependency-free PRNG (splitmix64). Same seed → same programs.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Rng { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

const NUM_TRANSFORMS: usize = 18;

/// Builds one program: file-scope `preamble` + `main` `body`, threading a carrier.
struct ProgBuilder {
    rng: Rng,
    preamble: Vec<String>, // file-scope struct/global decls
    body: Vec<String>,     // statements inside main
    var: usize,
    sct: usize,
    global: usize,
}

impl ProgBuilder {
    fn new(seed: u64) -> Self {
        ProgBuilder {
            rng: Rng::new(seed),
            preamble: Vec::new(),
            body: Vec::new(),
            var: 0,
            sct: 0,
            global: 0,
        }
    }

    fn fresh_var(&mut self) -> String {
        let v = format!("v{}", self.var);
        self.var += 1;
        v
    }

    /// Apply one transform to `cur`, returning the new carrier variable.
    fn transform(&mut self, cur: &str) -> String {
        let next = self.fresh_var();
        match self.rng.below(NUM_TRANSFORMS) {
            // taint-preserving
            0 => self.body.push(format!("int {next} = {cur};")),
            1 => self.body.push(format!("int {next} = id({cur});")), // direct call
            2 => {
                // indirect call through a function pointer (CTADL drops this — F1)
                let fp = self.fresh_var();
                self.body
                    .push(format!("int (*{fp})(int) = id; int {next} = {fp}({cur});"));
            }
            3 => {
                // store into and read back from a struct field
                let s = format!("S{}", self.sct);
                self.sct += 1;
                self.preamble.push(format!("struct {s} {{ int f; }};"));
                let obj = self.fresh_var();
                self.body.push(format!(
                    "struct {s} {obj}; {obj}.f = {cur}; int {next} = {obj}.f;"
                ));
            }
            4 => {
                // pointer round-trip
                let p = self.fresh_var();
                self.body
                    .push(format!("int *{p} = &{cur}; int {next} = *{p};"));
            }
            5 => {
                // global round-trip
                let g = format!("g{}", self.global);
                self.global += 1;
                self.preamble.push(format!("int {g};"));
                self.body.push(format!("{g} = {cur}; int {next} = {g};"));
            }
            6 => self.body.push(format!("int {next} = {cur} + 1;")), // arithmetic
            7 => {
                // array element round-trip (data flow through `a[i]`)
                let a = self.fresh_var();
                self.body
                    .push(format!("int {a}[3]; {a}[1] = {cur}; int {next} = {a}[1];"));
            }
            8 => {
                // indirect call through a function pointer in an ARRAY ELEMENT
                // (array + indirect combo). Elements are assigned individually here; the
                // brace-initialized spelling is transform 17, which must produce the same
                // facts. Keeping this one element-wise preserves it as the original F2
                // probe (funcptr-in-array-element taint, case 30).
                let fps = self.fresh_var();
                self.body.push(format!(
                    "int (*{fps}[2])(int); {fps}[0] = id; {fps}[1] = id; \
                     int {next} = {fps}[0]({cur});"
                ));
            }
            9 => {
                // indirect call through a POINTER to a function pointer (double indirection)
                let fp = self.fresh_var();
                let pp = self.fresh_var();
                self.body.push(format!(
                    "int (*{fp})(int) = id; int (**{pp})(int) = &{fp}; \
                     int {next} = (*{pp})({cur});"
                ));
            }
            10 => {
                // taint carried through a (taken) switch arm
                self.body.push(format!(
                    "int {next} = 0; switch (1) {{ case 1: {next} = {cur}; break; \
                     default: break; }}"
                ));
            }
            11 => {
                // goto skips a kill: at runtime taint is preserved (the `= 0` is
                // jumped over). The label is on a REAL statement (`{next} = {next};`):
                // a label on an empty statement (`L: ;`) is a known frontend gap
                // (case 32), so this form avoids it and exercises goto control flow.
                let lbl = format!("lbl_{}", self.fresh_var());
                self.body.push(format!(
                    "int {next} = {cur}; goto {lbl}; {next} = 0; {lbl}: {next} = {next};"
                ));
            }
            15 => {
                // ARRAY aggregate (brace) initializer, read back through a subscript. Was a
                // frontend gap (case 31) until spec 019 desugared `{...}` element-wise; these
                // three transforms are what keep that desugaring under the oracle.
                let a = self.fresh_var();
                self.body.push(format!(
                    "int {a}[3] = {{ {cur}, 0, 0 }}; int {next} = {a}[0];"
                ));
            }
            16 => {
                // STRUCT aggregate initializer: the elements map positionally onto the members,
                // so reading the member the tainted element initialized carries the taint. The
                // second (untainted) member is what makes a positional mis-map observable.
                let s = format!("S{}", self.sct);
                self.sct += 1;
                self.preamble
                    .push(format!("struct {s} {{ int f; int g; }};"));
                let obj = self.fresh_var();
                self.body.push(format!(
                    "struct {s} {obj} = {{ {cur}, 0 }}; int {next} = {obj}.f;"
                ));
            }
            17 => {
                // Brace-initialized funcptr ARRAY, then an indirect call through element 0 --
                // the shape that masked F2. Must produce exactly what transform 8's
                // element-assignment spelling does.
                let fps = self.fresh_var();
                self.body.push(format!(
                    "int (*{fps}[2])(int) = {{ id, id }}; int {next} = {fps}[0]({cur});"
                ));
            }
            // taint-killing / imprecision probes
            12 => self.body.push(format!("int {next} = 0;")), // kill (drops cur)
            13 => self
                .body
                .push(format!("int {next} = 0; if (1) {{ {next} = {cur}; }}")), // taken
            14 => self
                .body
                .push(format!("int {next} = 0; if (0) {{ {next} = {cur}; }}")), // dead branch
            _ => unreachable!(),
        }
        next
    }

    fn generate(mut self) -> String {
        let mut cur = self.fresh_var();
        self.body.push(format!("int {cur} = source();"));
        let steps = 1 + self.rng.below(6); // 1..=6 transforms
        for _ in 0..steps {
            cur = self.transform(&cur.clone());
        }
        self.body.push(format!("sink({cur});"));

        let mut s = String::new();
        s.push_str("/* generated by ctadl-dynamic gen */\n");
        s.push_str("int source();\nvoid sink(int);\nint id(int p) { return p; }\n");
        for d in &self.preamble {
            s.push_str(d);
            s.push('\n');
        }
        s.push_str("int main() {\n");
        for st in &self.body {
            s.push_str("    ");
            s.push_str(st);
            s.push('\n');
        }
        s.push_str("    return 0;\n}\n");
        s
    }
}

/// `gen <outdir> --count N [--seed S]`: write N generated `gen_NNNN.c` programs.
pub fn gen_mode(args: &[String]) -> Result<i32> {
    let outdir = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .context("usage: ctadl-dynamic gen <outdir> --count N [--seed S]")?;
    let count: usize = crate::arg_value(args, "--count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let seed: u64 = crate::arg_value(args, "--seed")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    std::fs::create_dir_all(outdir).with_context(|| format!("creating {outdir}"))?;
    let mut run = Rng::new(seed);
    for i in 0..count {
        let prog = ProgBuilder::new(run.next_u64()).generate();
        let path = Path::new(outdir).join(format!("gen_{i:04}.c"));
        std::fs::write(&path, prog).with_context(|| format!("writing {}", path.display()))?;
    }
    eprintln!("wrote {count} programs to {outdir}/ (seed {seed})");
    Ok(0)
}
