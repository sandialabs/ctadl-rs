/*! Class-merged lambdas, split back apart at import.

R8 merges classes of the same shape -- Kotlin lambdas, Java lambdas' synthetic classes -- into one
class with an extra `final int` field, the class id. Each constructor stores the id, and each
method that differed between the merged classes begins with a switch on it:

```text
.field public final synthetic i:I
<init>(Object, Object, Lf00;I)V:   iput p4, p0, Li;->i:I
r(Object)Object:                   iget v0, p0, Li;->i:I
                                   packed-switch v0, ...   # one arm per original class
```

Lowered as is, the switch is a nondeterministic branch, so each such method is the union of every
merged class's body, and every virtual call on a merged object dispatches to that union. On
`com.kgurgul.cpuinfo`, one class merges 29 lambdas, and its union summary lands at thirty call
sites; the index never converges.

This module finds such classes and the ids their objects are built with. The lowering in
`lib.rs` then gives each id `k` its own class, `C$r8id<k>`, a subclass of `C`, whose switching
methods are clones of `C`'s with each switch narrowed to arm `k`. A construction site whose id is
a known constant allocates `C$r8id<k>`, and dispatch on that object, by CHA or by the index's
call-target tags, reaches the clone. A site whose id is not known still allocates `C`, and `C`'s
own methods are kept, so nothing is lost.

This is sound because the field is final: the id an object is constructed with is the one every
method sees. R8 marks the field `synthetic` as well, but nothing here depends on that.
*/

use std::collections::{BTreeSet, VecDeque};

use hashbrown::hash_map::HashMap;
use streaming_iterator::StreamingIterator;

use dex_reader::DexParser;
use dex_reader::basic_blocks::{basic_blocks, block_successors};
use dex_reader::instructions::{Instruction, PayloadInstruction, Reg};
use dex_reader::parser::{DecodedCodeItem, decode_code_item};
use dex_reader::types::{ACC_FINAL, CodeItem};

/// The most ids a class is split into. A class built with more keeps its merged methods.
const MAX_IDS: usize = 64;

/// The merged classes of a program, and its construction sites whose id is known.
#[derive(Debug, Default)]
pub(crate) struct MergedClasses {
    /// By class descriptor.
    classes: HashMap<String, MergedClass>,
    /// By the signature of the method the site is in, then by the pc of its `new-instance`.
    sites: HashMap<String, HashMap<usize, Site>>,
}

#[derive(Debug)]
pub(crate) struct MergedClass {
    /// The class id field, as the index of its `field_id` in the dex that defines the class.
    field: u32,
    /// How each constructor sets the id, by constructor signature.
    ctors: HashMap<String, IdArg>,
    /// The switches on the id in each method that has any, by method signature.
    switched: HashMap<String, Vec<Switch>>,
    /// Every id an object of the class is built with, or a switch has an arm for.
    pub(crate) ids: BTreeSet<i64>,
}

/// Where a constructor gets the id it stores.
#[derive(Debug, Clone, Copy)]
enum IdArg {
    /// From its argument in this slot of the `invoke` (slot 0 is `this`).
    Slot(usize),
    /// A constant.
    Const(i64),
}

/// A switch on the class id: the pc of the switch, the pc each case jumps to, and the pc it
/// falls through to for any other id.
#[derive(Debug, Clone)]
pub(crate) struct Switch {
    pc: usize,
    arms: Vec<(i64, usize)>,
    default: usize,
}

impl Switch {
    /// The pc the switch jumps to for `id`.
    fn target(&self, id: i64) -> usize {
        self.arms
            .iter()
            .find(|(k, _)| *k == id)
            .map_or(self.default, |(_, pc)| *pc)
    }
}

/// A construction site of a merged class whose id is known.
#[derive(Debug, Clone)]
struct Site {
    class: String,
    id: SiteId,
}

#[derive(Debug, Clone, Copy)]
enum SiteId {
    Known(i64),
    /// The id of the enclosing method's own object: a merged class's method building another
    /// object of its class with its own id. Known in each clone, not in the merged method.
    Own,
}

impl MergedClasses {
    /// Finds the merged classes across every dex of a program. A class's construction sites can
    /// be in any dex, so this has to see them all before any is lowered.
    pub(crate) fn scan(parsers: &[&DexParser<'_>]) -> Self {
        let mut this = Self::default();
        for (dex, parser) in parsers.iter().enumerate() {
            if let Err(e) = this.scan_classes(parser) {
                log::warn!("merged classes: skipping dex {dex}: {e}");
            }
        }
        for (dex, parser) in parsers.iter().enumerate() {
            if let Err(e) = this.scan_sites(parser) {
                log::warn!("merged classes: skipping construction sites in dex {dex}: {e}");
            }
        }
        this.classes.retain(|name, c| {
            let keep = (2..=MAX_IDS).contains(&c.ids.len());
            if !keep {
                log::debug!("merged classes: not splitting {name}: {} ids", c.ids.len());
            }
            keep
        });
        let classes = &this.classes;
        for sites in this.sites.values_mut() {
            sites.retain(|_, s| classes.contains_key(&s.class));
        }
        this.sites.retain(|_, s| !s.is_empty());
        log::info!(
            "merged classes: {} split into {} ids, {} switching methods cloned into {} clones, {} \
             construction sites in {} methods",
            this.classes.len(),
            this.classes.values().map(|c| c.ids.len()).sum::<usize>(),
            this.classes.values().map(|c| c.switched.len()).sum::<usize>(),
            this.classes
                .values()
                .map(|c| c.ids.len() * c.switched.len())
                .sum::<usize>(),
            this.sites.values().map(|s| s.len()).sum::<usize>(),
            this.sites.len(),
        );
        this
    }

    pub(crate) fn class(&self, name: &str) -> Option<&MergedClass> {
        self.classes.get(name)
    }

    /// The class each `new-instance` in `method` allocates instead of the one it names, by pc:
    /// for the merged method itself (`own` is `None`), or for its clone for id `own`.
    pub(crate) fn retags(&self, method: &str, own: Option<i64>) -> HashMap<usize, String> {
        let Some(sites) = self.sites.get(method) else {
            return HashMap::new();
        };
        sites
            .iter()
            .filter_map(|(pc, s)| {
                let id = match s.id {
                    SiteId::Known(k) => k,
                    SiteId::Own => own?,
                };
                Some((*pc, split_class(&s.class, id)))
            })
            .collect()
    }

    /// Candidates: a class with a `final int` instance field that its constructors store and
    /// its methods switch on.
    fn scan_classes(&mut self, parser: &DexParser<'_>) -> Result<(), dex_reader::error::DexError> {
        let cp = parser.constant_pool();
        for class_def in parser.classes() {
            let class_data = parser.class_data(class_def)?;
            let fields: Vec<u32> = class_data
                .instance_fields
                .iter()
                .filter(|f| ACC_FINAL.is_set_in(f.access_flags))
                .filter(|f| {
                    parser
                        .get_field(f.field_idx as usize)
                        .and_then(|fid| fid.pretty_name(cp).ok())
                        .is_some_and(|n| n.ends_with(":I"))
                })
                .map(|f| f.field_idx)
                .collect();
            if fields.is_empty() {
                continue;
            }
            let class_name = parser.class_name(class_def)?;
            for field in fields {
                let mut class = MergedClass {
                    field,
                    ctors: HashMap::new(),
                    switched: HashMap::new(),
                    ids: BTreeSet::new(),
                };
                for enc in &class_data.direct_methods {
                    let Some(code) = parser.method_code(enc)? else {
                        continue;
                    };
                    let mi = parser.get_method(enc.method_idx as usize).unwrap();
                    if parser.method_name(mi)? != "<init>" {
                        continue;
                    }
                    let facts = analyze(parser, &code, Some(field));
                    // One store, from an argument or a constant. A constructor that delegates to
                    // another (`this(..)`) stores nothing itself; its sites keep the merged class.
                    let id = match facts.id_stores.as_slice() {
                        [Some(Val::Param(n))] if *n > 0 => IdArg::Slot(*n as usize),
                        [Some(Val::Const(c))] => IdArg::Const(*c),
                        _ => continue,
                    };
                    if let IdArg::Const(c) = id {
                        class.ids.insert(c);
                    }
                    class.ctors.insert(parser.method_signature(mi)?, id);
                }
                if class.ctors.is_empty() {
                    continue;
                }
                for enc in &class_data.virtual_methods {
                    let Some(code) = parser.method_code(enc)? else {
                        continue;
                    };
                    let facts = analyze(parser, &code, Some(field));
                    if facts.switches.is_empty() {
                        continue;
                    }
                    for s in &facts.switches {
                        class.ids.extend(s.arms.iter().map(|(k, _)| *k));
                    }
                    let mi = parser.get_method(enc.method_idx as usize).unwrap();
                    class
                        .switched
                        .insert(parser.method_signature(mi)?, facts.switches);
                }
                if !class.switched.is_empty() {
                    self.classes.insert(class_name, class);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Construction sites: a `new-instance` of a merged class whose constructor call passes a
    /// known id.
    fn scan_sites(&mut self, parser: &DexParser<'_>) -> Result<(), dex_reader::error::DexError> {
        let cp = parser.constant_pool();
        let allocates_merged = |inst: &Instruction| match inst {
            Instruction::NewInstance(f) => cp
                .type_ids
                .get(f.idx.0 as usize)
                .and_then(|t| t.descriptor(&cp.strings).ok())
                .is_some_and(|d| self.classes.contains_key(d.as_str())),
            _ => false,
        };
        let mut found = Vec::new();
        for class_def in parser.classes() {
            let class_data = parser.class_data(class_def)?;
            let own_field = self
                .classes
                .get(parser.class_name(class_def)?.as_str())
                .map(|c| c.field);
            for enc in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let Some(code) = parser.method_code(enc)? else {
                    continue;
                };
                let items = decode_code_item(&code);
                if !items.iter().any(|it| {
                    matches!(it, DecodedCodeItem::Instruction { inst, .. } if allocates_merged(inst))
                }) {
                    continue;
                }
                let facts = analyze_items(parser, &code, &items, own_field);
                let mi = parser.get_method(enc.method_idx as usize).unwrap();
                let sig = parser.method_signature(mi)?;
                for init in facts.inits {
                    let Some(class) = self.classes.get(&init.class) else {
                        continue;
                    };
                    let id = match class.ctors.get(&init.ctor) {
                        Some(IdArg::Const(c)) => SiteId::Known(*c),
                        Some(IdArg::Slot(n)) => match init.args.get(*n).copied().flatten() {
                            Some(Val::Const(k)) => SiteId::Known(k),
                            Some(Val::ClassId) => SiteId::Own,
                            _ => continue,
                        },
                        None => continue,
                    };
                    found.push((sig.clone(), init.new_pc, Site { class: init.class, id }));
                }
            }
        }
        for (sig, pc, site) in found {
            if let SiteId::Known(k) = site.id {
                self.classes.get_mut(&site.class).unwrap().ids.insert(k);
            }
            self.sites.entry(sig).or_default().insert(pc, site);
        }
        Ok(())
    }
}

impl MergedClass {
    /// The switches on the id in `method`, if it has any.
    pub(crate) fn switches(&self, method: &str) -> Option<&[Switch]> {
        self.switched.get(method).map(Vec::as_slice)
    }
}

/// For a clone for `id`: the pc each switch on the id jumps to, by the switch's pc.
pub(crate) fn narrowed(switches: &[Switch], id: i64) -> HashMap<usize, usize> {
    switches.iter().map(|s| (s.pc, s.target(id))).collect()
}

/// The class split off `class` for `id`: `Li;` and 29 give `Li$r8id29;`.
pub(crate) fn split_class(class: &str, id: i64) -> String {
    let stem = class.strip_suffix(';').unwrap_or(class);
    if id < 0 {
        format!("{stem}$r8idm{};", id.unsigned_abs())
    } else {
        format!("{stem}$r8id{id};")
    }
}

/// `method`'s signature with its class replaced by `class`.
pub(crate) fn split_method(method: &str, class: &str) -> String {
    match method.split_once("->") {
        Some((_, rest)) => format!("{class}->{rest}"),
        None => method.to_string(),
    }
}

/// A register's value, where the analysis knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Val {
    /// The incoming argument in this slot; slot 0 is `this` in an instance method.
    Param(u16),
    Const(i64),
    /// `this`'s class id.
    ClassId,
    /// The object allocated by the `new-instance` at this pc.
    New(usize),
}

/// Registers to values; a register not in the map has an unknown value.
type State = HashMap<u32, Val>;

/// A constructor call on a freshly allocated object.
#[derive(Debug)]
struct Init {
    new_pc: usize,
    class: String,
    ctor: String,
    /// The value of each argument register, `this` first.
    args: Vec<Option<Val>>,
}

#[derive(Debug, Default)]
struct Facts {
    /// The value of every store to `this`'s class id field.
    id_stores: Vec<Option<Val>>,
    /// The switches on `this`'s class id.
    switches: Vec<Switch>,
    inits: Vec<Init>,
}

fn analyze(parser: &DexParser<'_>, code: &CodeItem, field: Option<u32>) -> Facts {
    analyze_items(parser, code, &decode_code_item(code), field)
}

/// A must-analysis of register values over the method's blocks, then one pass over each block
/// to record what the values are at the instructions of interest. `field` is the class id field
/// of the method's own class, if that class is a candidate.
///
/// Within a try block every instruction that can throw ends its block, so an exception edge
/// leaves from the last instruction, before its effect. A block's out-state is therefore the
/// meet of the states before and after its last instruction, which is sound for its normal and
/// its exception successors alike.
fn analyze_items(
    parser: &DexParser<'_>,
    code: &CodeItem,
    items: &[DecodedCodeItem],
    field: Option<u32>,
) -> Facts {
    let blocks = basic_blocks(code, items);
    let succs = block_successors(code, items);
    let mut facts = Facts::default();
    if blocks.is_empty() {
        return facts;
    }
    let mut ins: Vec<Option<State>> = vec![None; blocks.len()];
    let first = code.first_param_reg();
    ins[0] = Some(
        (first..code.registers_size)
            .map(|r| (u32::from(r), Val::Param(r - first)))
            .collect(),
    );
    let mut work: VecDeque<usize> = VecDeque::from([0]);
    while let Some(b) = work.pop_front() {
        let mut state = ins[b].clone().unwrap();
        let mut before_last = state.clone();
        for it in &items[blocks[b].start..blocks[b].end] {
            if let DecodedCodeItem::Instruction { offset, inst } = it {
                before_last = state.clone();
                step(parser, items, inst, *offset, &mut state, field, None);
            }
        }
        let out = meet(&before_last, &state);
        for &s in &succs[b] {
            let next = match &ins[s] {
                None => out.clone(),
                Some(cur) => meet(cur, &out),
            };
            if ins[s].as_ref() != Some(&next) {
                ins[s] = Some(next);
                work.push_back(s);
            }
        }
    }
    for (b, block) in blocks.iter().enumerate() {
        let Some(mut state) = ins[b].clone() else {
            continue;
        };
        for it in &items[block.start..block.end] {
            if let DecodedCodeItem::Instruction { offset, inst } = it {
                step(
                    parser,
                    items,
                    inst,
                    *offset,
                    &mut state,
                    field,
                    Some(&mut facts),
                );
            }
        }
    }
    facts
}

fn meet(a: &State, b: &State) -> State {
    a.iter()
        .filter(|(r, v)| b.get(*r) == Some(*v))
        .map(|(r, v)| (*r, *v))
        .collect()
}

/// One instruction's effect on `state`, recording into `facts` when given.
fn step(
    parser: &DexParser<'_>,
    items: &[DecodedCodeItem],
    inst: &Instruction,
    pc: usize,
    state: &mut State,
    field: Option<u32>,
    facts: Option<&mut Facts>,
) {
    let get = |state: &State, r: Reg| state.get(&r.0).copied();
    let set = |state: &mut State, r: Reg, v: Option<Val>| match v {
        Some(v) => state.insert(r.0, v),
        None => state.remove(&r.0),
    };
    let is_field = |idx: u32| field == Some(idx);
    match inst {
        Instruction::Const4(f) => _ = set(state, f.a, Some(Val::Const(f.lit.into()))),
        Instruction::Const16(f) => _ = set(state, f.a, Some(Val::Const(f.lit.into()))),
        Instruction::Const(f) => _ = set(state, f.a, Some(Val::Const(f.lit.into()))),
        Instruction::ConstHigh16(f) => {
            _ = set(state, f.a, Some(Val::Const(((f.lit as i32) << 16).into())))
        }
        Instruction::Move(f) | Instruction::MoveObject(f) => {
            _ = set(state, f.a, get(state, f.b))
        }
        Instruction::MoveFrom16(f) | Instruction::MoveObjectFrom16(f) => {
            _ = set(state, f.a, get(state, f.b))
        }
        Instruction::Move16(f) | Instruction::MoveObject16(f) => {
            _ = set(state, f.a, get(state, f.b))
        }
        Instruction::IGet(f) if is_field(f.idx.0) && get(state, f.b) == Some(Val::Param(0)) => {
            _ = set(state, f.a, Some(Val::ClassId))
        }
        Instruction::IPut(f) => {
            if let Some(facts) = facts
                && is_field(f.idx.0)
                && get(state, f.b) == Some(Val::Param(0))
            {
                facts.id_stores.push(get(state, f.a));
            }
        }
        // A store writes no register. (`data_flow` lists `iput-wide`'s object as a destination.)
        Instruction::IPutWide(_)
        | Instruction::IPutObject(_)
        | Instruction::IPutBoolean(_)
        | Instruction::IPutByte(_)
        | Instruction::IPutChar(_)
        | Instruction::IPutShort(_) => {}
        Instruction::NewInstance(f) => _ = set(state, f.a, Some(Val::New(pc))),
        // A cast leaves the value alone.
        Instruction::CheckCast(_) => {}
        Instruction::PackedSwitch(f) | Instruction::SparseSwitch(f) => {
            if let Some(facts) = facts
                && get(state, f.a) == Some(Val::ClassId)
                && let Some(switch) = switch_at(items, pc, f.tgt)
            {
                facts.switches.push(switch);
            }
        }
        Instruction::InvokeDirect(_) | Instruction::InvokeDirectRange(_) => {
            let (idx, args) = match inst {
                Instruction::InvokeDirect(f) => (f.idx, f.args.as_slice()),
                Instruction::InvokeDirectRange(f) => (f.idx, f.args.as_slice()),
                _ => unreachable!(),
            };
            if let Some(facts) = facts
                && let Some(&recv) = args.first()
                && let Some(Val::New(new_pc)) = get(state, recv)
                && let Some(mi) = parser.get_method(idx.0 as usize)
                && let Ok((class, name, _)) = parser.method_triple(mi)
                && name == "<init>"
                && let Ok(ctor) = parser.method_signature(mi)
            {
                facts.inits.push(Init {
                    new_pc,
                    class,
                    ctor,
                    args: args.iter().map(|r| get(state, *r)).collect(),
                });
            }
            // The receiver is now initialized, but it is still the same object.
        }
        _ => {
            let mut dest = inst.data_flow().dest;
            while let Some(r) = dest.next() {
                state.remove(&r.0);
            }
        }
    }
}

/// The switch at `pc` whose payload is at `pc + tgt`.
fn switch_at(items: &[DecodedCodeItem], pc: usize, tgt: i32) -> Option<Switch> {
    let payload_pc = usize::try_from(pc as i64 + i64::from(tgt)).ok()?;
    let at = |rel: i32| usize::try_from(pc as i64 + i64::from(rel)).ok();
    let i = items.iter().position(|it| it.offset() == pc)?;
    let default = items.get(i + 1)?.offset();
    let arms = items.iter().find_map(|it| match it {
        DecodedCodeItem::Payload { offset, payload } if *offset == payload_pc => Some(payload),
        _ => None,
    })?;
    let arms: Vec<(i64, usize)> = match arms {
        PayloadInstruction::PackedSwitch(p) => p
            .targets
            .iter()
            .enumerate()
            .filter_map(|(i, rel)| Some((i64::from(p.first_key) + i as i64, at(*rel)?)))
            .collect(),
        PayloadInstruction::SparseSwitch(p) => p
            .keys
            .iter()
            .zip(&p.targets)
            .filter_map(|(k, rel)| Some((i64::from(*k), at(*rel)?)))
            .collect(),
        PayloadInstruction::FillArrayData(_) => return None,
    };
    Some(Switch { pc, arms, default })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_names() {
        assert_eq!(split_class("Li;", 29), "Li$r8id29;");
        assert_eq!(split_class("La/B$C;", -1), "La/B$C$r8idm1;");
        assert_eq!(
            split_method("Li;->r(Ljava/lang/Object;)Ljava/lang/Object;", "Li$r8id3;"),
            "Li$r8id3;->r(Ljava/lang/Object;)Ljava/lang/Object;"
        );
    }

    #[test]
    fn switch_takes_its_arm_or_falls_through() {
        let s = Switch {
            pc: 4,
            arms: vec![(0, 10), (1, 20), (5, 30)],
            default: 6,
        };
        assert_eq!(s.target(1), 20);
        assert_eq!(s.target(5), 30);
        assert_eq!(s.target(2), 6);
        assert_eq!(narrowed(&[s], 0), HashMap::from([(4, 10)]));
    }

    #[test]
    fn meet_keeps_agreeing_registers() {
        let a: State = [(0, Val::Const(1)), (1, Val::ClassId), (2, Val::Param(0))].into();
        let b: State = [(0, Val::Const(2)), (1, Val::ClassId)].into();
        assert_eq!(meet(&a, &b), [(1, Val::ClassId)].into());
    }
}
