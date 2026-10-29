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
merged class's body, and every virtual call on a merged object dispatches to that union. A class
can merge dozens of lambdas, and its union's summary then lands at every call site that reaches
it, enough that the index may not converge.

This module finds such classes and the ids their objects are built with. The lowering in
`lib.rs` then gives each id `k` its own class, `C$r8id<k>`, a subclass of `C`, whose switching
methods are clones of `C`'s with each switch narrowed to arm `k`. A construction site whose id is
a known constant allocates `C$r8id<k>`, and dispatch on that object, by CHA or by the index's
call-target tags, reaches the clone. A site whose id is not known still allocates `C`, and `C`'s
own methods are kept, so nothing is lost.

This is sound because the field is final: the id an object is constructed with is the one every
method sees. R8 marks the field `synthetic` as well, but nothing here depends on that.
*/

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use hashbrown::hash_map::HashMap;

use ctadl_import::notifications::{
    DEFAULT_MESSAGE, Level, MERGED_CLASSES_NOT_SPLIT, MERGED_CLASSES_SPLIT, Notifications,
};

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
    /// What the split did, for the guardrail; set by [`MergedClasses::scan`].
    counts: Counts,
    /// The guardrail's checks that failed in [`MergedClasses::scan`].
    failed: Vec<Check>,
}

/// What the split of a program's merged classes did.
#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    /// Classes split.
    split: usize,
    /// Split classes not closed: their merged methods stay live.
    open: usize,
    /// Classes with more than [`MAX_IDS`] ids, left merged.
    over_cap: usize,
    /// `new-instance`s of split classes, and those whose id is not known.
    allocs: usize,
    unresolved: usize,
}

/// A failed guardrail check: what it says, without the `merged classes: ` prefix of its log line,
/// and the counts behind it.
#[derive(Debug, Clone)]
struct Check {
    /// Which of [`MERGED_CLASSES_NOT_SPLIT`]'s messages, and the arguments that fill it.
    message_id: &'static str,
    arguments: Vec<String>,
    properties: BTreeMap<String, serde_json::Value>,
}

impl Check {
    fn new(message_id: &'static str, arguments: &[usize], counts: &[(&str, usize)]) -> Self {
        Self {
            message_id,
            arguments: arguments.iter().map(ToString::to_string).collect(),
            properties: properties(counts),
        }
    }

    fn text(&self) -> String {
        MERGED_CLASSES_NOT_SPLIT.render(self.message_id, &self.arguments)
    }
}

/// `counts` as notification properties.
fn properties(counts: &[(&str, usize)]) -> BTreeMap<String, serde_json::Value> {
    counts
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
        .collect()
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
    /// Whether the class is `final`: then nothing but its own clones subclass it, and no
    /// `invoke-super` reaches its methods.
    final_class: bool,
    /// Every `new-instance` of the class in the program, and those that allocate a clone.
    allocs: usize,
    known_allocs: usize,
    /// Whether no object of the class itself can exist; see [`MergedClass::empties_union`].
    closed: bool,
}

impl MergedClass {
    /// Whether `method`, a merged method of this class, is lowered with an empty body. When the
    /// class is `final` and every `new-instance` of it allocates a clone, no object is ever the
    /// class itself, so the merged method never runs; only the clones' do. Emptying it keeps a
    /// call resolved by class hierarchy, which reaches the merged method beside the clones,
    /// from taking on the union of every arm. It stays a target, so a call's targets, and with
    /// them whether the index resolves it by class hierarchy or by its call-target tags, do not
    /// change. An object built by reflection is not seen.
    pub(crate) fn empties_union(&self, method: &str) -> bool {
        self.closed && self.switched.contains_key(method)
    }
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
        let mut over_cap = 0;
        this.classes.retain(|name, c| {
            let keep = (2..=MAX_IDS).contains(&c.ids.len());
            over_cap += usize::from(c.ids.len() > MAX_IDS);
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
        for (name, c) in this.classes.iter_mut() {
            c.closed = c.final_class && c.allocs == c.known_allocs;
            if !c.closed {
                log::debug!(
                    "merged classes: keeping {name}'s merged methods: final={}, {} of {} allocations \
                     allocate a clone",
                    c.final_class,
                    c.known_allocs,
                    c.allocs
                );
            }
        }
        log::info!(
            "merged classes: {} of {} have no object of their own, so their merged methods get empty \
             bodies",
            this.classes.values().filter(|c| c.closed).count(),
            this.classes.len()
        );
        this.counts = Counts {
            split: this.classes.len(),
            open: this.classes.values().filter(|c| !c.closed).count(),
            over_cap,
            allocs: this.classes.values().map(|c| c.allocs).sum(),
            unresolved: this
                .classes
                .values()
                .map(|c| c.allocs.saturating_sub(c.known_allocs))
                .sum(),
        };
        this.failed = this.guardrail();
        log::info!(
            "merged classes: {} split into {} ids, {} switching methods cloned into {} clones, {} \
             construction sites in {} methods",
            this.classes.len(),
            this.classes.values().map(|c| c.ids.len()).sum::<usize>(),
            this.classes
                .values()
                .map(|c| c.switched.len())
                .sum::<usize>(),
            this.classes
                .values()
                .map(|c| c.ids.len() * c.switched.len())
                .sum::<usize>(),
            this.sites.values().map(|s| s.len()).sum::<usize>(),
            this.sites.len(),
        );
        this
    }

    /// Warns when the split is visibly not working: merged classes it cannot split or close, or
    /// objects it cannot tell the id of. Each of these leaves merged methods, the union of every
    /// arm, live, and with them false flows across the merged lambdas. The thresholds sit between
    /// the apps measured with the split working (2-4% of classes not closed, about 0.1% of
    /// allocations unresolved) and with it not working (12-15%, 2-4%). Returns the checks that
    /// failed.
    fn guardrail(&self) -> Vec<Check> {
        let Counts {
            split,
            open,
            over_cap,
            allocs,
            unresolved,
        } = self.counts;
        log::info!(
            "merged classes guardrail: {split} split, {open} not closed, {over_cap} over {MAX_IDS} \
             ids, {unresolved} of {allocs} allocations with no known id"
        );
        let mut failed = Vec::new();
        if over_cap > 0 && over_cap * 10 > split + over_cap {
            failed.push(Check::new(
                "overCap",
                &[over_cap, MAX_IDS],
                &[("overCap", over_cap), ("split", split)],
            ));
        }
        if split >= 20 && open * 10 > split {
            failed.push(Check::new(
                "notClosed",
                &[open, split],
                &[("notClosed", open), ("split", split)],
            ));
        }
        if allocs >= 100 && unresolved * 50 > allocs {
            failed.push(Check::new(
                "unresolvedAllocations",
                &[unresolved, allocs],
                &[
                    ("unresolvedAllocations", unresolved),
                    ("allocations", allocs),
                ],
            ));
        }
        for check in &failed {
            log::warn!("merged classes: {}", check.text());
        }
        failed
    }

    /// The guardrail's notifications, none for a program with no merged classes: one
    /// [`MERGED_CLASSES_SPLIT`] with the split's counts, then one [`MERGED_CLASSES_NOT_SPLIT`] per
    /// failed check. `clone_statements` is the statements in the clones of merged methods;
    /// when there are any, this also checks that they are a modest share of the program's
    /// `statements`. Each clone stores only its own arm, so a large share means they are storing
    /// the whole merged body again; with the arms pruned they are 2-24% of the apps measured.
    pub(crate) fn guardrail_notifications(
        &self,
        clone_statements: usize,
        statements: impl FnOnce() -> usize,
    ) -> Notifications {
        let mut out = Notifications::default();
        let Counts {
            split,
            open,
            over_cap,
            allocs,
            unresolved,
        } = self.counts;
        if split + over_cap == 0 {
            return out;
        }
        let mut counts = properties(&[
            ("split", split),
            ("notClosed", open),
            ("overCap", over_cap),
            ("unresolvedAllocations", unresolved),
            ("allocations", allocs),
        ]);
        let mut failed = self.failed.clone();
        if clone_statements > 0 {
            let total = statements();
            log::info!(
                "merged classes guardrail: clones hold {clone_statements} of {total} statements"
            );
            let clones = [("cloneStatements", clone_statements), ("statements", total)];
            counts.extend(properties(&clones));
            if clone_statements * 100 > total * 35 {
                let check = Check::new("cloneStatements", &[clone_statements, total], &clones);
                log::warn!("merged classes: {}", check.text());
                failed.push(check);
            }
        }
        out.push(
            &MERGED_CLASSES_SPLIT,
            DEFAULT_MESSAGE,
            Level::Note,
            vec![split.to_string()],
            counts,
        );
        for check in failed {
            out.push(
                &MERGED_CLASSES_NOT_SPLIT,
                check.message_id,
                Level::Warning,
                check.arguments,
                check.properties,
            );
        }
        out
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
                    final_class: ACC_FINAL.is_set_in(class_def.access_flags),
                    allocs: 0,
                    known_allocs: 0,
                    closed: false,
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
        let mut allocs: HashMap<String, usize> = HashMap::new();
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
                let mut any = false;
                for it in &items {
                    if let DecodedCodeItem::Instruction {
                        inst: inst @ Instruction::NewInstance(f),
                        ..
                    } = it
                        && allocates_merged(inst)
                        && let Some(d) = cp
                            .type_ids
                            .get(f.idx.0 as usize)
                            .and_then(|t| t.descriptor(&cp.strings).ok())
                    {
                        *allocs.entry(d).or_default() += 1;
                        any = true;
                    }
                }
                if !any {
                    continue;
                }
                let facts = analyze_items(parser, &code, &items, own_field);
                let mi = parser.get_method(enc.method_idx as usize).unwrap();
                let sig = parser.method_signature(mi)?;
                if log::log_enabled!(log::Level::Debug) {
                    for it in &items {
                        if let DecodedCodeItem::Instruction { inst, offset } = it
                            && allocates_merged(inst)
                            && !facts.inits.iter().any(|i| i.new_pc == *offset)
                        {
                            log::debug!(
                                "merged classes: no constructor call found in {sig} at {offset}"
                            );
                        }
                    }
                }
                // The class each `new-instance` allocates, by pc, and how many constructor calls
                // initialize it.
                let allocated: HashMap<usize, String> = items
                    .iter()
                    .filter_map(|it| match it {
                        DecodedCodeItem::Instruction {
                            inst: Instruction::NewInstance(f),
                            offset,
                        } => cp
                            .type_ids
                            .get(f.idx.0 as usize)
                            .and_then(|t| t.descriptor(&cp.strings).ok())
                            .map(|d| (*offset, d)),
                        _ => None,
                    })
                    .collect();
                let mut inits_at: HashMap<usize, usize> = HashMap::new();
                for init in &facts.inits {
                    *inits_at.entry(init.new_pc).or_default() += 1;
                }
                for init in facts.inits {
                    let Some(class) = self.classes.get(&init.class) else {
                        continue;
                    };
                    // A constructor call is a site of its class only when the object is of that
                    // class: R8 drops a subclass's constructor and initializes the subclass's
                    // object with its superclass's (`new-instance S; invoke-direct S's super
                    // <init>`), and retagging that object would make it a clone of the
                    // superclass. An object two constructor calls could initialize is not a site
                    // either.
                    if allocated.get(&init.new_pc) != Some(&init.class)
                        || inits_at[&init.new_pc] > 1
                    {
                        log::debug!(
                            "merged classes: {} in {sig} at {} is not a site of {}",
                            allocated.get(&init.new_pc).map_or("?", String::as_str),
                            init.new_pc,
                            init.class
                        );
                        continue;
                    }
                    let id = match class.ctors.get(&init.ctor) {
                        Some(IdArg::Const(c)) => SiteId::Known(*c),
                        Some(IdArg::Slot(n)) => match init.args.get(*n).copied().flatten() {
                            Some(Val::Const(k)) => SiteId::Known(k),
                            Some(Val::ClassId) => SiteId::Own,
                            other => {
                                log::debug!(
                                    "merged classes: unknown id for {} in {sig} at {}: {other:?}",
                                    init.class,
                                    init.new_pc
                                );
                                continue;
                            }
                        },
                        None => {
                            log::debug!(
                                "merged classes: unrecognized constructor {} for {} in {sig} at {}",
                                init.ctor,
                                init.class,
                                init.new_pc
                            );
                            continue;
                        }
                    };
                    found.push((
                        sig.clone(),
                        init.new_pc,
                        Site {
                            class: init.class,
                            id,
                        },
                    ));
                }
            }
        }
        for (class, n) in allocs {
            self.classes.get_mut(&class).unwrap().allocs += n;
        }
        for (sig, pc, site) in found {
            let class = self.classes.get_mut(&site.class).unwrap();
            // An object built with its enclosing method's own id is a clone only where that
            // method is cloned: a switching method of the class (`switched` holds only those).
            let known = match site.id {
                SiteId::Known(_) => true,
                SiteId::Own => class.switched.contains_key(&sig),
            };
            if known {
                class.known_allocs += 1;
            }
            if let SiteId::Known(k) = site.id {
                class.ids.insert(k);
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
        let mut last_allocates = false;
        for it in &items[blocks[b].start..blocks[b].end] {
            if let DecodedCodeItem::Instruction { offset, inst } = it {
                before_last = state.clone();
                last_allocates = matches!(inst, Instruction::NewInstance(_));
                step(parser, items, inst, *offset, &mut state, field, None);
            }
        }
        // A block's last instruction may throw, and then its result never reaches a handler, so
        // a successor sees only what holds either way. A `new-instance` is the exception: inside
        // a `try` it ends its block, and the handler can never see the object, since the verifier
        // invalidates uninitialized references on exception edges. Dropping it would lose the
        // constructor call in the next block, and with it the site's id.
        let out = if last_allocates {
            state.clone()
        } else {
            meet(&before_last, &state)
        };
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
        Instruction::Move(f) | Instruction::MoveObject(f) => _ = set(state, f.a, get(state, f.b)),
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
            for r in inst.data_flow().dest {
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

    fn with_counts(counts: Counts) -> MergedClasses {
        let mut merged = MergedClasses {
            counts,
            ..Default::default()
        };
        merged.failed = merged.guardrail();
        merged
    }

    #[test]
    fn no_merged_classes_no_notifications() {
        let merged = with_counts(Counts::default());
        assert!(merged.guardrail_notifications(0, || 0).is_empty());
    }

    #[test]
    fn guardrail_notes_the_split_and_each_failed_check() {
        use ctadl_import::notifications::{descriptor_id, property_u64, text};
        let merged = with_counts(Counts {
            split: 30,
            open: 10,
            over_cap: 1,
            allocs: 200,
            unresolved: 1,
        });
        let n = merged.guardrail_notifications(40, || 100);

        let split: Vec<_> = n.of(&MERGED_CLASSES_SPLIT).collect();
        assert_eq!(split.len(), 1);
        let split = split[0];
        assert_eq!(text(split), "split 30 merged classes");
        assert_eq!(split.level, Some(serde_json::json!("note")));
        for (key, value) in [
            ("split", 30),
            ("notClosed", 10),
            ("overCap", 1),
            ("unresolvedAllocations", 1),
            ("allocations", 200),
            ("cloneStatements", 40),
            ("statements", 100),
        ] {
            assert_eq!(property_u64(split, key), Some(value), "{key}");
        }

        // Too many classes not closed, and clones over 35% of the program; one class over the
        // cap of 31 and 1 unresolved of 200 allocations are under their thresholds.
        let failed: Vec<_> = n.of(&MERGED_CLASSES_NOT_SPLIT).collect();
        assert_eq!(failed.len(), 2);
        assert!(
            failed
                .iter()
                .all(|f| f.level == Some(serde_json::json!("warning")))
        );
        assert!(text(failed[0]).starts_with("10 of 30 split classes"));
        assert_eq!(property_u64(failed[0], "notClosed"), Some(10));
        assert!(text(failed[1]).starts_with("the clones of merged methods hold 40"));
        assert_eq!(property_u64(failed[1], "cloneStatements"), Some(40));
        assert_eq!(property_u64(failed[1], "statements"), Some(100));
        assert_eq!(
            descriptor_id(&n.notifications()[0]),
            Some(MERGED_CLASSES_SPLIT.id)
        );
    }

    #[test]
    fn without_clones_the_split_has_no_clone_counts() {
        use ctadl_import::notifications::property_u64;
        let merged = with_counts(Counts {
            split: 2,
            allocs: 4,
            ..Default::default()
        });
        let mut statements_counted = false;
        let n = merged.guardrail_notifications(0, || {
            statements_counted = true;
            0
        });
        assert!(!statements_counted);
        assert_eq!(n.len(), 1);
        let split = &n.notifications()[0];
        assert_eq!(property_u64(split, "split"), Some(2));
        assert_eq!(property_u64(split, "cloneStatements"), None);
        assert_eq!(property_u64(split, "statements"), None);
    }

    #[test]
    fn meet_keeps_agreeing_registers() {
        let a: State = [(0, Val::Const(1)), (1, Val::ClassId), (2, Val::Param(0))].into();
        let b: State = [(0, Val::Const(2)), (1, Val::ClassId)].into();
        assert_eq!(meet(&a, &b), [(1, Val::ClassId)].into());
    }
}
