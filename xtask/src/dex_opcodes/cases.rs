//! The per-opcode cases of `cargo xtask dex-opcodes`.
//!
//! Each case names one instruction under test and the type of every register it touches, taken
//! from the Dalvik bytecode spec, not from dex-reader. The generator loads every other register
//! v0..v13 with an int, so a perturbation can reach any register; v14 and v15 belong to the
//! harness. Operands are vA = v1, vB = v4, vC = v7 wherever the format allows, so a wide operand's
//! pair never overlaps another operand.

use std::collections::BTreeMap;

/// The registers a case may load and observe. v14 and v15 are the harness's own.
pub const CASE_REGS: u32 = 14;
/// An int argument that makes every callee throw (`H.MAGIC`).
pub const MAGIC: i32 = 0xDEAD;

/// A register's value before the instruction, with the candidates the driver tries: the first is
/// the baseline, the rest are perturbations.
#[derive(Clone, Debug)]
pub enum In {
    I(Vec<i32>),
    F(Vec<f32>),
    /// A long in vN, vN+1.
    J(Vec<i64>),
    /// A double in vN, vN+1.
    D(Vec<f64>),
    /// An object from a driver domain, cast to `desc` (`@SELF@` is the case's own class).
    L {
        desc: &'static str,
        domain: &'static str,
        count: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    I,
    F,
    J,
    D,
    L,
}

impl Ty {
    pub fn is_wide(self) -> bool {
        matches!(self, Ty::J | Ty::D)
    }

    pub fn letter(self) -> char {
        match self {
            Ty::I => 'I',
            Ty::F => 'F',
            Ty::J => 'J',
            Ty::D => 'D',
            Ty::L => 'L',
        }
    }
}

impl In {
    pub fn ty(&self) -> Ty {
        match self {
            In::I(_) => Ty::I,
            In::F(_) => Ty::F,
            In::J(_) => Ty::J,
            In::D(_) => Ty::D,
            In::L { .. } => Ty::L,
        }
    }
}

/// A value a fault run assigns to one register.
#[derive(Clone, Copy, Debug)]
pub enum Fault {
    Int(i32),
    Long(i64),
    Null,
    /// The k'th object of the register's domain; 9 is a length-1 array.
    Obj(u32),
}

#[derive(Clone, Debug, Default)]
pub struct Case {
    pub name: String,
    /// The instruction under test, in smali.
    pub insn: String,
    pub ins: BTreeMap<u32, In>,
    /// Registers the instruction writes, with their type after. Others keep their type.
    pub writes: Vec<(u32, Ty)>,
    /// Lines before the instruction under test (after the register loads).
    pub prelude: Vec<String>,
    /// Lines after it, before the register stores.
    pub postlude: Vec<String>,
    /// Payloads, after the method's last return.
    pub payload: Vec<String>,
    /// For the return opcodes: the method's return type (`V`, `I`, `J` or `L`) and descriptor.
    pub ret: Option<(char, &'static str)>,
    /// Registers left unobserved after the instruction (an uninitialized reference cannot be
    /// stored).
    pub hidden: Vec<u32>,
    /// Registers the harness itself writes after the instruction, observed but not checked.
    pub harness: Vec<(u32, Ty)>,
    /// Input assignments under which the instruction must throw.
    pub faults: Vec<Vec<(u32, Fault)>>,
    /// The instruction cannot run without throwing (a missing class, a zero literal divisor):
    /// only `can_throw()` is checked.
    pub fault_only: bool,
}

impl Case {
    fn new(name: impl Into<String>, insn: impl Into<String>) -> Self {
        Case {
            name: name.into(),
            insn: insn.into(),
            ..Case::default()
        }
    }

    /// The opcode mnemonic of the instruction under test.
    pub fn mnemonic(&self) -> &str {
        self.insn.split_whitespace().next().unwrap_or("")
    }

    fn i(mut self, reg: u32, v: In) -> Self {
        self.ins.insert(reg, v);
        self
    }

    fn w(mut self, reg: u32, ty: Ty) -> Self {
        self.writes.push((reg, ty));
        self
    }

    fn pre(mut self, lines: &[&str]) -> Self {
        self.prelude.extend(lines.iter().map(|s| s.to_string()));
        self
    }

    fn post(mut self, lines: &[&str]) -> Self {
        self.postlude.extend(lines.iter().map(|s| s.to_string()));
        self
    }

    fn payload(mut self, lines: &[&str]) -> Self {
        self.payload.extend(lines.iter().map(|s| s.to_string()));
        self
    }

    fn hide(mut self, reg: u32) -> Self {
        self.hidden.push(reg);
        self
    }

    fn harness(mut self, reg: u32, ty: Ty) -> Self {
        self.harness.push((reg, ty));
        self
    }

    fn fault(mut self, f: &[(u32, Fault)]) -> Self {
        self.faults.push(f.to_vec());
        self
    }

    fn ret(mut self, letter: char, desc: &'static str) -> Self {
        self.ret = Some((letter, desc));
        self
    }

    fn fault_only(mut self) -> Self {
        self.fault_only = true;
        self
    }
}

// --- candidate values -------------------------------------------------------

/// Distinct, nonzero ints for register `r`: a baseline and three perturbations that differ from
/// it in low and high bits alike.
pub fn ints(r: u32) -> In {
    let base = (0x0101_0101_i32).wrapping_mul(r as i32 + 3) ^ 0x2468;
    In::I(vec![
        base,
        base ^ 0x5a5a_5a5a,
        base.wrapping_neg().wrapping_add(11),
        0x7fff_fff1 - r as i32,
    ])
}

fn longs(r: u32) -> In {
    let base = 0x0123_4567_89ab_cdef_i64 ^ (i64::from(r) * 0x1111_1111_1111);
    In::J(vec![base, -base, 0x7654_3210_0fed_cba9])
}

fn floats(r: u32) -> In {
    In::F(vec![1_234_567.0 + r as f32, -7654.25, 3.5e9, 0.75])
}

fn doubles(r: u32) -> In {
    // Near 2^30, so the low word holds integer bits too: double-to-int sees a change in either.
    In::D(vec![1_073_741_825.5 + f64::from(r), -2.5e10, 7.25, 1e100])
}

fn generic(ty: Ty, r: u32) -> In {
    match ty {
        Ty::I => ints(r),
        Ty::F => floats(r),
        Ty::J => longs(r),
        Ty::D => doubles(r),
        Ty::L => obj(),
    }
}

/// Divisors: small, so a quotient or remainder depends on both operands.
fn divisor(ty: Ty) -> In {
    match ty {
        Ty::J => In::J(vec![3, 7, -5, 1000]),
        _ => In::I(vec![3, 7, -5, 1000]),
    }
}

fn idx() -> In {
    In::I(vec![0, 1, 2])
}

fn shift() -> In {
    In::I(vec![3, 7, 12])
}

fn obj() -> In {
    In::L {
        desc: "LH$Obj;",
        domain: "obj",
        count: 3,
    }
}

fn str_() -> In {
    In::L {
        desc: "Ljava/lang/String;",
        domain: "str",
        count: 3,
    }
}

fn any() -> In {
    // k = 0 is a String, 1 an H$Obj, 2 an Integer.
    In::L {
        desc: "Ljava/lang/Object;",
        domain: "any",
        count: 3,
    }
}

fn array(desc: &'static str, domain: &'static str) -> In {
    In::L {
        desc,
        domain,
        count: 3,
    }
}

fn this_class() -> In {
    In::L {
        desc: "L@SELF@;",
        domain: "self",
        count: 3,
    }
}

const NULL: Fault = Fault::Null;

const BRANCH_POST: &[&str] = &[
    "const/4 v15, 0",
    "goto :out",
    ":taken",
    "const/4 v15, 1",
    ":out",
];

// --- the cases --------------------------------------------------------------

pub fn all() -> Vec<Case> {
    let mut cases = Vec::new();
    moves(&mut cases);
    returns(&mut cases);
    consts(&mut cases);
    objects(&mut cases);
    branches(&mut cases);
    arrays(&mut cases);
    fields(&mut cases);
    invokes(&mut cases);
    arithmetic(&mut cases);
    missing_references(&mut cases);
    cases
}

fn moves(cases: &mut Vec<Case>) {
    for op in ["move", "move/from16", "move/16"] {
        cases.push(Case::new(op, format!("{op} v1, v4")).w(1, Ty::I));
    }
    for op in ["move-wide", "move-wide/from16", "move-wide/16"] {
        cases.push(
            Case::new(op, format!("{op} v1, v4"))
                .i(4, longs(4))
                .w(1, Ty::J),
        );
    }
    // The spec allows a wide move's source and destination pairs to overlap.
    cases.push(
        Case::new("move-wide overlapping", "move-wide v5, v4")
            .i(4, longs(4))
            .w(5, Ty::J),
    );
    for op in ["move-object", "move-object/from16", "move-object/16"] {
        cases.push(
            Case::new(op, format!("{op} v1, v4"))
                .i(1, obj())
                .i(4, obj())
                .w(1, Ty::L),
        );
    }
    cases.push(
        Case::new("move-result", "move-result v1")
            .pre(&["invoke-static {}, LH;->retI()I"])
            .w(1, Ty::I),
    );
    cases.push(
        Case::new("move-result-wide", "move-result-wide v1")
            .pre(&["invoke-static {}, LH;->retJ()J"])
            .w(1, Ty::J),
    );
    cases.push(
        Case::new("move-result-object", "move-result-object v1")
            .pre(&["invoke-static {}, LH;->retL()Ljava/lang/Object;"])
            .w(1, Ty::L),
    );
    cases.push(
        Case::new("move-exception", "move-exception v1")
            .pre(&[
                "sget-object v14, LH;->exc:Ljava/lang/Throwable;",
                ":try_start",
                "throw v14",
                ":try_end",
                ".catch Ljava/lang/Throwable; {:try_start .. :try_end} :handler",
                ":handler",
            ])
            .w(1, Ty::L),
    );
}

fn returns(cases: &mut Vec<Case>) {
    cases.push(Case::new("return-void", "return-void").ret('V', "V"));
    cases.push(Case::new("return", "return v1").ret('I', "I"));
    cases.push(
        Case::new("return-wide", "return-wide v1")
            .i(1, longs(1))
            .ret('J', "J"),
    );
    cases.push(
        Case::new("return-object", "return-object v1")
            .i(1, obj())
            .ret('L', "Ljava/lang/Object;"),
    );
}

fn consts(cases: &mut Vec<Case>) {
    for (op, lit, ty) in [
        ("const/4", "5", Ty::I),
        ("const/16", "1234", Ty::I),
        ("const", "0x12345678", Ty::I),
        ("const/high16", "0x12340000", Ty::I),
        ("const-wide/16", "1234", Ty::J),
        ("const-wide/32", "0x123456", Ty::J),
        ("const-wide", "0x123456789abcL", Ty::J),
        ("const-wide/high16", "0x1234000000000000L", Ty::J),
        ("const-string", "\"s\"", Ty::L),
        ("const-string/jumbo", "\"s\"", Ty::L),
        ("const-class", "LH$Obj;", Ty::L),
        ("const-method-handle", "invoke-static@LH;->mh0(II)V", Ty::L),
        ("const-method-type", "(II)V", Ty::L),
    ] {
        cases.push(Case::new(op, format!("{op} v1, {lit}")).w(1, ty));
    }
}

fn objects(cases: &mut Vec<Case>) {
    cases.push(
        Case::new("monitor-enter", "monitor-enter v1")
            .i(1, obj())
            .post(&["monitor-exit v1"])
            .fault(&[(1, NULL)]),
    );
    cases.push(
        Case::new("monitor-exit", "monitor-exit v1")
            .i(1, obj())
            .pre(&["monitor-enter v1"]),
    );
    cases.push(
        Case::new("check-cast", "check-cast v1, Ljava/lang/String;")
            .i(1, any())
            .w(1, Ty::L)
            .fault(&[(1, Fault::Obj(1))]),
    );
    cases.push(
        Case::new("instance-of", "instance-of v1, v4, Ljava/lang/String;")
            .i(4, any())
            .w(1, Ty::I),
    );
    cases.push(
        Case::new("array-length", "array-length v1, v4")
            .i(4, array("[I", "[I"))
            .w(1, Ty::I)
            .fault(&[(4, NULL)]),
    );
    cases.push(
        Case::new("new-instance", "new-instance v1, LH$Obj;")
            .post(&["invoke-direct {v1}, LH$Obj;-><init>()V"])
            .w(1, Ty::L),
    );
    cases.push(
        Case::new("new-array", "new-array v1, v4, [I")
            .i(4, In::I(vec![3, 1, 5]))
            .w(1, Ty::L)
            .fault(&[(4, Fault::Int(-1))]),
    );
    cases.push(
        Case::new("filled-new-array", "filled-new-array {v1, v4, v7}, [I")
            .post(&["move-result-object v15"])
            .harness(15, Ty::L),
    );
    cases.push(
        Case::new(
            "filled-new-array/range",
            "filled-new-array/range {v4 .. v6}, [I",
        )
        .post(&["move-result-object v15"])
        .harness(15, Ty::L),
    );
    cases.push(
        Case::new("fill-array-data", "fill-array-data v1, :data")
            .i(1, array("[I", "[I"))
            .payload(&[
                ":data",
                ".array-data 4",
                "    0x1 0x2 0x3",
                ".end array-data",
            ])
            .fault(&[(1, NULL)])
            .fault(&[(1, Fault::Obj(9))]),
    );
    cases.push(
        Case::new("throw", "throw v1")
            .i(
                1,
                In::L {
                    desc: "Ljava/lang/Throwable;",
                    domain: "throwable",
                    count: 3,
                },
            )
            .fault(&[(1, NULL)]),
    );
}

fn branches(cases: &mut Vec<Case>) {
    for op in ["goto", "goto/16", "goto/32"] {
        cases.push(Case::new(op, format!("{op} :target")).post(&[":target"]));
    }
    cases.push(
        Case::new("packed-switch", "packed-switch v1, :switch")
            .i(1, In::I(vec![0, 1, 7]))
            .post(&[
                "const/4 v15, 0",
                "goto :out",
                ":c0",
                "const/4 v15, 1",
                "goto :out",
                ":c1",
                "const/4 v15, 2",
                ":out",
            ])
            .payload(&[
                ":switch",
                ".packed-switch 0x0",
                "    :c0",
                "    :c1",
                ".end packed-switch",
            ])
            .harness(15, Ty::I),
    );
    cases.push(
        Case::new("sparse-switch", "sparse-switch v1, :switch")
            .i(1, In::I(vec![0x10, 0x20, 7]))
            .post(&[
                "const/4 v15, 0",
                "goto :out",
                ":c0",
                "const/4 v15, 1",
                "goto :out",
                ":c1",
                "const/4 v15, 2",
                ":out",
            ])
            .payload(&[
                ":switch",
                ".sparse-switch",
                "    0x10 -> :c0",
                "    0x20 -> :c1",
                ".end sparse-switch",
            ])
            .harness(15, Ty::I),
    );
    // Comparisons start from equal operands, so a change to any slot of either one shows.
    let equal_f = || In::F(vec![1.5, 1.25, 1.75]);
    let equal_d = || doubles(0);
    let equal_j = || longs(0);
    for op in ["cmpl-float", "cmpg-float"] {
        cases.push(
            Case::new(op, format!("{op} v1, v4, v7"))
                .i(4, equal_f())
                .i(7, equal_f())
                .w(1, Ty::I),
        );
    }
    for op in ["cmpl-double", "cmpg-double"] {
        cases.push(
            Case::new(op, format!("{op} v1, v4, v7"))
                .i(4, equal_d())
                .i(7, equal_d())
                .w(1, Ty::I),
        );
    }
    cases.push(
        Case::new("cmp-long", "cmp-long v1, v4, v7")
            .i(4, equal_j())
            .i(7, equal_j())
            .w(1, Ty::I),
    );
    for op in ["if-eq", "if-ne", "if-lt", "if-ge", "if-gt", "if-le"] {
        cases.push(
            Case::new(op, format!("{op} v1, v4, :taken"))
                .i(1, In::I(vec![100, 99, 101]))
                .i(4, In::I(vec![100, 99, 101]))
                .post(BRANCH_POST)
                .harness(15, Ty::I),
        );
    }
    for op in ["if-eqz", "if-nez", "if-ltz", "if-gez", "if-gtz", "if-lez"] {
        cases.push(
            Case::new(op, format!("{op} v1, :taken"))
                .i(1, In::I(vec![0, -1, 1]))
                .post(BRANCH_POST)
                .harness(15, Ty::I),
        );
    }
}

fn arrays(cases: &mut Vec<Case>) {
    // (opcode suffix, array descriptor, driver domain, element type, value candidates)
    let kinds: Vec<(&str, &'static str, &'static str, Ty, Option<In>)> = vec![
        ("", "[I", "[I", Ty::I, None),
        ("", "[F", "[F", Ty::F, None),
        ("-wide", "[J", "[J", Ty::J, None),
        ("-wide", "[D", "[D", Ty::D, None),
        ("-boolean", "[Z", "[Z", Ty::I, Some(In::I(vec![1, 0]))),
        ("-byte", "[B", "[B", Ty::I, Some(In::I(vec![5, -3, 77]))),
        ("-char", "[C", "[C", Ty::I, Some(In::I(vec![65, 66, 90]))),
        ("-short", "[S", "[S", Ty::I, Some(In::I(vec![300, -300, 7]))),
    ];
    for (suffix, desc, domain, ty, value) in kinds {
        let name = |op: &str| {
            if matches!(desc, "[F" | "[D") {
                format!("{op}{suffix} {desc}")
            } else {
                format!("{op}{suffix}")
            }
        };
        cases.push(
            Case::new(name("aget"), format!("aget{suffix} v1, v4, v7"))
                .i(4, array(desc, domain))
                .i(7, idx())
                .w(1, ty)
                .fault(&[(4, NULL)])
                .fault(&[(7, Fault::Int(5))]),
        );
        cases.push(
            Case::new(name("aput"), format!("aput{suffix} v1, v4, v7"))
                .i(1, value.unwrap_or_else(|| generic(ty, 1)))
                .i(4, array(desc, domain))
                .i(7, idx())
                .fault(&[(4, NULL)])
                .fault(&[(7, Fault::Int(5))]),
        );
    }
    cases.push(
        Case::new("aget-object", "aget-object v1, v4, v7")
            .i(4, array("[Ljava/lang/Object;", "[Object"))
            .i(7, idx())
            .w(1, Ty::L)
            .fault(&[(4, NULL)])
            .fault(&[(7, Fault::Int(5))]),
    );
    // Into a String[], so storing a non-String (any#1) raises ArrayStoreException.
    cases.push(
        Case::new("aput-object", "aput-object v1, v4, v7")
            .i(1, any())
            .i(4, array("[Ljava/lang/String;", "[String"))
            .i(7, idx())
            .fault(&[(4, NULL)])
            .fault(&[(7, Fault::Int(5))])
            .fault(&[(1, Fault::Obj(1))]),
    );
}

/// The fields of `H$Obj` and `H$S`, by opcode suffix: (suffix, name, descriptor, type, values).
fn field_kinds() -> Vec<(&'static str, &'static str, &'static str, Ty, In)> {
    vec![
        ("", "i", "I", Ty::I, ints(1)),
        ("-wide", "j", "J", Ty::J, longs(1)),
        ("-object", "o", "Ljava/lang/Object;", Ty::L, str_()),
        ("-boolean", "z", "Z", Ty::I, In::I(vec![1, 0])),
        ("-byte", "b", "B", Ty::I, In::I(vec![5, -3, 77])),
        ("-char", "c", "C", Ty::I, In::I(vec![65, 66, 90])),
        ("-short", "s", "S", Ty::I, In::I(vec![300, -300, 7])),
    ]
}

fn fields(cases: &mut Vec<Case>) {
    for (suffix, name, desc, ty, value) in field_kinds() {
        cases.push(
            Case::new(
                format!("iget{suffix}"),
                format!("iget{suffix} v1, v4, LH$Obj;->{name}:{desc}"),
            )
            .i(4, obj())
            .w(1, ty)
            .fault(&[(4, NULL)]),
        );
        cases.push(
            Case::new(
                format!("iput{suffix}"),
                format!("iput{suffix} v1, v4, LH$Obj;->{name}:{desc}"),
            )
            .i(1, value.clone())
            .i(4, obj())
            .fault(&[(4, NULL)]),
        );
        cases.push(
            Case::new(
                format!("sget{suffix}"),
                format!("sget{suffix} v1, LH$S;->{name}:{desc}"),
            )
            .w(1, ty),
        );
        cases.push(
            Case::new(
                format!("sput{suffix}"),
                format!("sput{suffix} v1, LH$S;->{name}:{desc}"),
            )
            .i(1, value),
        );
    }
    // The 32-bit and 64-bit opcodes also serve float and double fields.
    cases.push(Case::new("sget F", "sget v1, LH$S;->f:F").w(1, Ty::F));
    cases.push(Case::new("sget-wide D", "sget-wide v1, LH$S;->d:D").w(1, Ty::D));
    cases.push(Case::new("sput F", "sput v1, LH$S;->f:F").i(1, floats(1)));
    cases.push(Case::new("sput-wide D", "sput-wide v1, LH$S;->d:D").i(1, doubles(1)));
}

fn invokes(cases: &mut Vec<Case>) {
    // Receiver in v1, then an int, a long pair and an object, so each argument kind shows.
    let sig = "(IJLjava/lang/Object;)V";
    let receivers: [(&str, In, String); 4] = [
        ("invoke-virtual", obj(), format!("LH$Obj;->v{sig}")),
        ("invoke-super", this_class(), format!("LH$Obj;->v{sig}")),
        ("invoke-direct", this_class(), format!("L@SELF@;->p{sig}")),
        (
            "invoke-interface",
            In::L {
                desc: "LH$Iface;",
                domain: "obj",
                count: 3,
            },
            format!("LH$Iface;->v{sig}"),
        ),
    ];
    for (op, receiver, method) in receivers {
        for range in [false, true] {
            let (name, regs) = if range {
                (format!("{op}/range"), "{v1 .. v5}")
            } else {
                (op.to_string(), "{v1, v2, v3, v4, v5}")
            };
            cases.push(
                Case::new(name.clone(), format!("{name} {regs}, {method}"))
                    .i(1, receiver.clone())
                    .i(2, ints(2))
                    .i(3, longs(3))
                    .i(5, str_())
                    .fault(&[(1, NULL)])
                    .fault(&[(2, Fault::Int(MAGIC))]),
            );
        }
    }
    for range in [false, true] {
        let (name, regs) = if range {
            ("invoke-static/range", "{v1 .. v5}")
        } else {
            ("invoke-static", "{v1, v2, v3, v4, v5}")
        };
        cases.push(
            Case::new(
                name,
                format!("{name} {regs}, LH;->s(IIJLjava/lang/Object;)V"),
            )
            .i(3, longs(3))
            .i(5, str_())
            .fault(&[(1, Fault::Int(MAGIC))]),
        );
    }
    let mh = In::L {
        desc: "Ljava/lang/invoke/MethodHandle;",
        domain: "mh",
        count: 2,
    };
    for (name, regs) in [
        ("invoke-polymorphic", "{v1, v2, v3}"),
        ("invoke-polymorphic/range", "{v1 .. v3}"),
    ] {
        cases.push(
            Case::new(
                name,
                format!(
                    "{name} {regs}, Ljava/lang/invoke/MethodHandle;->invokeExact([Ljava/lang/Object;)Ljava/lang/Object;, (II)V"
                ),
            )
            .i(1, mh.clone())
            .fault(&[(1, NULL)])
            .fault(&[(2, Fault::Int(MAGIC))]),
        );
    }
    let call_site = "call_site_0(\"cs\", (II)V)@LH;->bsm(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";
    for (name, regs) in [
        ("invoke-custom", "{v1, v2}"),
        ("invoke-custom/range", "{v1 .. v2}"),
    ] {
        cases.push(
            Case::new(name, format!("{name} {regs}, {call_site}")).fault(&[(1, Fault::Int(MAGIC))]),
        );
    }
}

fn arithmetic(cases: &mut Vec<Case>) {
    use Ty::*;
    for (op, dst, src) in [
        ("neg-int", I, I),
        ("not-int", I, I),
        ("neg-long", J, J),
        ("not-long", J, J),
        ("neg-float", F, F),
        ("neg-double", D, D),
        ("int-to-long", J, I),
        ("int-to-float", F, I),
        ("int-to-double", D, I),
        ("long-to-int", I, J),
        ("long-to-float", F, J),
        ("long-to-double", D, J),
        ("float-to-int", I, F),
        ("float-to-long", J, F),
        ("float-to-double", D, F),
        ("double-to-int", I, D),
        ("double-to-long", J, D),
        ("double-to-float", F, D),
        ("int-to-byte", I, I),
        ("int-to-char", I, I),
        ("int-to-short", I, I),
    ] {
        // A float keeps 24 bits, so only a small long shows a change to its low word.
        let source = if op == "long-to-float" {
            In::J(vec![1_234_567, -7, 0x7fff_ffff])
        } else {
            generic(src, 4)
        };
        cases.push(Case::new(op, format!("{op} v1, v4")).i(4, source).w(1, dst));
    }

    let int_ops = [
        "add", "sub", "mul", "div", "rem", "and", "or", "xor", "shl", "shr", "ushr",
    ];
    let float_ops = ["add", "sub", "mul", "div", "rem"];
    let typed: Vec<(&str, Ty, &[&str])> = vec![
        ("int", I, &int_ops),
        ("long", J, &int_ops),
        ("float", F, &float_ops),
        ("double", D, &float_ops),
    ];
    for (t, ty, ops) in typed {
        for &o in ops {
            let is_shift = o.starts_with("sh") || o == "ushr";
            let divides = matches!(o, "div" | "rem") && matches!(ty, I | J);
            let amount = |r| {
                if is_shift {
                    shift()
                } else if divides {
                    divisor(ty)
                } else {
                    generic(ty, r)
                }
            };
            let zero = match ty {
                J => vec![(7, Fault::Long(0))],
                _ => vec![(7, Fault::Int(0))],
            };
            let op = format!("{o}-{t}");
            let mut c = Case::new(op.clone(), format!("{op} v1, v4, v7"))
                .i(4, generic(ty, 4))
                .i(7, amount(7))
                .w(1, ty);
            if divides {
                c = c.fault(&zero);
            }
            cases.push(c);

            let op2 = format!("{o}-{t}/2addr");
            let mut c = Case::new(op2.clone(), format!("{op2} v1, v7"))
                .i(1, generic(ty, 1))
                .i(7, amount(7))
                .w(1, ty);
            if divides {
                c = c.fault(&zero);
            }
            cases.push(c);
        }
    }

    for (op, lit) in [
        ("add-int/lit16", "0x123"),
        ("rsub-int", "0x123"),
        ("mul-int/lit16", "0x123"),
        ("div-int/lit16", "0x123"),
        ("rem-int/lit16", "0x123"),
        ("and-int/lit16", "0x123"),
        ("or-int/lit16", "0x123"),
        ("xor-int/lit16", "0x123"),
        ("add-int/lit8", "13"),
        ("rsub-int/lit8", "13"),
        ("mul-int/lit8", "13"),
        ("div-int/lit8", "13"),
        ("rem-int/lit8", "13"),
        ("and-int/lit8", "13"),
        ("or-int/lit8", "13"),
        ("xor-int/lit8", "13"),
        ("shl-int/lit8", "13"),
        ("shr-int/lit8", "13"),
        ("ushr-int/lit8", "13"),
    ] {
        cases.push(Case::new(op, format!("{op} v1, v4, {lit}")).w(1, I));
    }
    for op in [
        "div-int/lit16",
        "rem-int/lit16",
        "div-int/lit8",
        "rem-int/lit8",
    ] {
        cases.push(Case::new(format!("{op} by zero"), format!("{op} v1, v4, 0")).fault_only());
    }
}

/// Instructions whose only fault is a reference that does not resolve.
fn missing_references(cases: &mut Vec<Case>) {
    for (name, insn) in [
        ("const-class missing", "const-class v1, LMissing;"),
        ("check-cast missing", "check-cast v1, LMissing;"),
        ("instance-of missing", "instance-of v1, v4, LMissing;"),
        ("new-instance missing", "new-instance v1, LMissing;"),
        ("new-array missing", "new-array v1, v7, [LMissing;"),
        (
            "filled-new-array missing",
            "filled-new-array {v1}, [LMissing;",
        ),
        (
            "filled-new-array/range missing",
            "filled-new-array/range {v1 .. v1}, [LMissing;",
        ),
        ("sget missing", "sget v1, LH$S;->missing:I"),
        ("sput missing", "sput v1, LH$S;->missing:I"),
        ("iget missing", "iget v1, v4, LH$Obj;->missing:I"),
        ("iput missing", "iput v1, v4, LH$Obj;->missing:I"),
        (
            "const-method-handle missing",
            "const-method-handle v1, invoke-static@LH;->missing()V",
        ),
        (
            "const-method-type missing",
            "const-method-type v1, (LMissing;)V",
        ),
        ("monitor-exit unlocked", "monitor-exit v4"),
    ] {
        let mut c = Case::new(name, insn).fault_only();
        if insn.contains("v4") {
            c = c.i(4, obj());
        }
        if insn.contains("check-cast") {
            c = c.i(1, any());
        }
        if insn.contains("new-array") {
            c = c.i(7, In::I(vec![2]));
        }
        if insn.starts_with("filled-new-array") {
            c = c.i(1, any());
        }
        // Declare what the instruction writes, so the stores after it verify.
        if insn.starts_with("new-instance") {
            c = c.hide(1);
        }
        let writes_object = ["const-class", "new-array", "const-method"]
            .iter()
            .any(|p| insn.starts_with(p));
        if writes_object {
            c = c.w(1, Ty::L);
        }
        cases.push(c);
    }
}
