use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::codegen::RETURN_INDEX;
use crate::error::Error;
use crate::facts::{
    FlowVariable, FlowVertex, FormalIndex, FormalType, Function, FunctionId, IdMap, InsnId,
    InsnSiteId, IntentKind, Path, Str,
};
use crate::index_engine::{IndexFacts, source_info::IndexSourceInfo};
use crate::languages::android_manifest::AndroidManifest;
use crate::project::AnalysisProject;
use ctadl_ir::{ProgramInfo, call::VirtualMethodTable};

const INTENT: &str = "Landroid/content/Intent;";
const BUNDLE: &str = "Landroid/os/Bundle;";
const COMPONENT_NAME: &str = "Landroid/content/ComponentName;";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AndroidIntentStats {
    pub api_functions: usize,
    pub api_summary_rows: usize,
    pub intent_frames: usize,
    pub keyed_extra_sites: usize,
    pub lumped_extra_sites: usize,
}

#[derive(Debug, Default)]
pub struct AndroidIntentObserver {
    parents: BTreeMap<String, Vec<String>>,
}

impl AndroidIntentObserver {
    pub fn observe_import(&mut self, program_info: &ProgramInfo) {
        if let VirtualMethodTable::Java { hierarchy, .. } = &program_info.vmt {
            for (subclass, parents) in hierarchy {
                self.parents
                    .entry(subclass.0.to_string())
                    .or_default()
                    .extend(parents.iter().map(|parent| parent.0.to_string()));
            }
        }
    }
}

pub fn emit_phase2_facts(facts: &mut IndexFacts, ids: &IdMap) -> AndroidIntentStats {
    let mut stats = emit_api_summaries(facts, ids);
    stats.intent_frames = emit_intent_frames(facts, ids);
    emit_extras_assigns(facts, ids, &mut stats);
    stats
}

pub fn emit_phase3_facts(
    project: &AnalysisProject,
    observer: &AndroidIntentObserver,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> Result<AndroidIntentStats, Error> {
    let mut stats = AndroidIntentStats::default();
    let filters_and_components = load_manifest_receivers(project, observer, &source_info.sites)?;
    facts.intent_filter.extend(filters_and_components.filters);
    facts
        .intent_component
        .extend(filters_and_components.components);
    emit_send_sites(facts, source_info, &mut stats);
    Ok(stats)
}

fn emit_api_summaries(facts: &mut IndexFacts, ids: &IdMap) -> AndroidIntentStats {
    let mut summaries = BTreeSet::new();
    let mut formals = BTreeSet::new();
    let mut api_functions = BTreeSet::new();

    for (func_id, function) in ids.functions() {
        let Some(sig) = JavaSig::parse(function.0.as_ref()) else {
            continue;
        };
        for (dst, dst_path, src, src_path) in api_rows(sig) {
            api_functions.insert(func_id);
            formals.insert((func_id, FlowVariable::formal_index(dst)));
            formals.insert((func_id, FlowVariable::formal_index(src)));
            summaries.insert((func_id, dst, dst_path, src, src_path));
        }
    }

    let api_summary_rows = summaries.len();
    facts.formal_param.extend(
        formals
            .into_iter()
            .map(|(func_id, var)| (func_id, var, FormalType::ByRef)),
    );
    facts.summary.extend(summaries);

    AndroidIntentStats {
        api_functions: api_functions.len(),
        api_summary_rows,
        ..Default::default()
    }
}

fn emit_intent_frames(facts: &mut IndexFacts, ids: &IdMap) -> usize {
    let api_functions: BTreeSet<_> = ids
        .functions()
        .filter_map(|(func_id, function)| {
            JavaSig::parse(function.0.as_ref()).and_then(|sig| {
                if is_intent_api(sig) || is_send_api(sig) {
                    Some(func_id)
                } else {
                    None
                }
            })
        })
        .collect();

    let frames: BTreeSet<_> = facts
        .call
        .iter()
        .filter_map(|(site, target)| {
            if api_functions.contains(target) {
                let site = InsnSiteId::try_from(*site).ok()?;
                Some((site.func_id,))
            } else {
                None
            }
        })
        .collect();
    let count = frames.len();
    facts.intent_frame.extend(frames);
    count
}

fn emit_send_sites(
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
    stats: &mut AndroidIntentStats,
) {
    let activity_intent_paths: Vec<Path> = facts
        .paths
        .iter()
        .map(|(p,)| path("<intent>").concat(p))
        .collect();
    let mut sends = BTreeSet::new();
    let mut assigns = BTreeSet::new();
    let mut actuals = BTreeSet::new();
    let mut frames = BTreeSet::new();
    for (site, cls, name, descriptor) in &facts.android_call_site {
        if is_local_broadcast_manager(cls.as_ref()) {
            continue;
        }
        let Some(kind) = send_kind(name.as_ref(), descriptor.as_ref()) else {
            continue;
        };
        let InsnSiteId { func_id, insn_id } = InsnSiteId::try_from(*site).unwrap();
        let bridge = source_info.add_insn_site(func_id);
        let bridge_site = bridge.try_into().unwrap();
        let intent_arg = FormalIndex::new(1);
        let intent = call_arg_vertex(insn_id, intent_arg);
        sends.insert((func_id, bridge.insn_id, insn_id, intent_arg, kind));
        frames.insert((func_id,));
        match kind {
            IntentKind::Activity => {
                actuals.insert((
                    bridge_site,
                    FormalIndex::new(0),
                    FlowVertex(
                        call_arg_var(bridge.insn_id, FormalIndex::new(0)),
                        Path::empty(),
                    ),
                ));
                actuals.insert((bridge_site, FormalIndex::new(1), intent.clone()));
                assigns.insert((
                    bridge_site,
                    FlowVertex(
                        call_arg_var(bridge.insn_id, FormalIndex::new(0)),
                        path("<intent>"),
                    ),
                    intent.clone(),
                ));
                assigns.insert((
                    bridge_site,
                    FlowVertex(
                        call_arg_var(bridge.insn_id, FormalIndex::new(1)),
                        Path::empty(),
                    ),
                    intent,
                ));
            }
            IntentKind::Receiver => {
                actuals.insert((bridge_site, FormalIndex::new(2), intent.clone()));
                assigns.insert((
                    bridge_site,
                    FlowVertex(
                        call_arg_var(bridge.insn_id, FormalIndex::new(2)),
                        Path::empty(),
                    ),
                    intent,
                ));
            }
            IntentKind::StartedService | IntentKind::BoundService => {
                actuals.insert((bridge_site, FormalIndex::new(1), intent.clone()));
                assigns.insert((
                    bridge_site,
                    FlowVertex(
                        call_arg_var(bridge.insn_id, FormalIndex::new(1)),
                        Path::empty(),
                    ),
                    intent,
                ));
            }
        }
    }
    stats.intent_frames += frames.len();
    facts.intent_frame.extend(frames);
    facts.intent_send.extend(sends);
    facts.actual_param.extend(actuals);
    facts.assign.extend(assigns);
    facts
        .paths
        .extend(activity_intent_paths.into_iter().map(|p| (p,)));
}

fn load_manifest_receivers(
    project: &AnalysisProject,
    observer: &AndroidIntentObserver,
    ids: &IdMap,
) -> Result<ManifestReceivers, Error> {
    let mut out = ManifestReceivers::default();
    for import in project.iter_imports() {
        let import = import?;
        let manifest_path = import
            .import_path()
            .join(crate::facts::schema::manifest_node::FILENAME);
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = AndroidManifest::load(import.import_path())?;
        for component in manifest.components() {
            let Some(descriptor) = component.descriptor.as_deref() else {
                continue;
            };
            let kinds = kinds_for_tag(&component.tag);
            if kinds.is_empty() {
                continue;
            }
            for kind in kinds {
                let receiver_descriptor =
                    component.target_activity.as_deref().unwrap_or(descriptor);
                let entries = entry_methods(ids, observer, receiver_descriptor, kind);
                for recv in entries {
                    out.components.push((Str::from(descriptor), kind, recv));
                    if receiver_descriptor != descriptor {
                        out.components
                            .push((Str::from(receiver_descriptor), kind, recv));
                    }
                    if let Some(dotted) = descriptor_to_dotted(descriptor) {
                        out.components.push((Str::from(dotted), kind, recv));
                    }
                    for filter in filters_for_component_node(&manifest, component.node_id, kind) {
                        for action in filter.actions {
                            if filter.mime_types.is_empty() {
                                out.filters
                                    .push((Str::from(action), Str::from(""), kind, recv));
                            } else {
                                for mime_type in &filter.mime_types {
                                    out.filters.push((
                                        Str::from(action.clone()),
                                        Str::from(mime_type.clone()),
                                        kind,
                                        recv,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out.components.sort_unstable();
    out.components.dedup();
    out.filters.sort_unstable();
    out.filters.dedup();
    Ok(out)
}

#[derive(Default)]
struct ManifestReceivers {
    filters: Vec<(Str, Str, IntentKind, FunctionId)>,
    components: Vec<(Str, IntentKind, FunctionId)>,
}

fn entry_methods(
    ids: &IdMap,
    observer: &AndroidIntentObserver,
    descriptor: &str,
    kind: IntentKind,
) -> Vec<FunctionId> {
    let methods: &[(&str, &str)] = match kind {
        IntentKind::Activity => &[
            ("onCreate", "(Landroid/os/Bundle;)V"),
            ("onNewIntent", "(Landroid/content/Intent;)V"),
            ("onStart", "()V"),
            ("onResume", "()V"),
        ],
        IntentKind::Receiver => &[(
            "onReceive",
            "(Landroid/content/Context;Landroid/content/Intent;)V",
        )],
        IntentKind::StartedService => &[
            ("onStartCommand", "(Landroid/content/Intent;II)I"),
            ("onHandleIntent", "(Landroid/content/Intent;)V"),
        ],
        IntentKind::BoundService => &[("onBind", "(Landroid/content/Intent;)Landroid/os/IBinder;")],
    };
    methods
        .iter()
        .filter_map(|(name, desc)| entry_method(ids, observer, descriptor, name, desc))
        .collect()
}

fn entry_method(
    ids: &IdMap,
    observer: &AndroidIntentObserver,
    descriptor: &str,
    name: &str,
    desc: &str,
) -> Option<FunctionId> {
    let mut queue = VecDeque::from([descriptor.to_string()]);
    let mut seen = BTreeSet::new();
    while let Some(class) = queue.pop_front() {
        if !seen.insert(class.clone()) {
            continue;
        }
        let method = Function(format!("{class}->{name}{desc}").into());
        if let Some(func_id) = ids.get_function_id(method) {
            return Some(func_id);
        }
        if let Some(parents) = observer.parents.get(&class) {
            queue.extend(parents.iter().cloned());
        }
    }
    None
}

#[derive(Default)]
struct ManifestFilter {
    actions: Vec<String>,
    mime_types: Vec<String>,
}

fn filters_for_component_node(
    manifest: &AndroidManifest,
    component_id: u32,
    kind: IntentKind,
) -> Vec<ManifestFilter> {
    let mut out = Vec::new();
    for filter in manifest
        .children
        .iter()
        .filter(|child| child.parent_id == component_id)
    {
        if manifest
            .nodes
            .get(filter.child_id as usize)
            .map(|n| n.tag.as_str())
            != Some("intent-filter")
        {
            continue;
        }
        if kind == IntentKind::Activity
            && !filter_has_category(manifest, filter.child_id, "android.intent.category.DEFAULT")
        {
            continue;
        }
        let mut filter_out = ManifestFilter::default();
        for child in manifest
            .children
            .iter()
            .filter(|child| child.parent_id == filter.child_id)
        {
            let tag = manifest
                .nodes
                .get(child.child_id as usize)
                .map(|n| n.tag.as_str());
            match tag {
                Some("action") => {
                    if let Some(action) = manifest.attrs.iter().find(|attr| {
                        attr.node_id == child.child_id && attr_key_matches(&attr.key, "name")
                    }) {
                        filter_out.actions.push(action.value.clone());
                    }
                }
                Some("data") => {
                    if let Some(mime_type) = manifest.attrs.iter().find(|attr| {
                        attr.node_id == child.child_id && attr_key_matches(&attr.key, "mimeType")
                    }) {
                        filter_out.mime_types.push(mime_type.value.clone());
                    }
                }
                _ => {}
            }
        }
        out.push(filter_out);
    }
    out
}

fn filter_has_category(manifest: &AndroidManifest, filter_id: u32, category: &str) -> bool {
    manifest
        .children
        .iter()
        .filter(|child| child.parent_id == filter_id)
        .any(|child| {
            manifest
                .nodes
                .get(child.child_id as usize)
                .is_some_and(|node| node.tag == "category")
                && manifest.attrs.iter().any(|attr| {
                    attr.node_id == child.child_id
                        && attr_key_matches(&attr.key, "name")
                        && attr.value == category
                })
        })
}

fn attr_key_matches(key: &str, name: &str) -> bool {
    key == name || key == format!("android:{name}") || key.ends_with(&format!(":{name}"))
}

fn kinds_for_tag(tag: &str) -> Vec<IntentKind> {
    match tag {
        "activity" | "activity-alias" => vec![IntentKind::Activity],
        "receiver" => vec![IntentKind::Receiver],
        "service" => vec![IntentKind::StartedService, IntentKind::BoundService],
        _ => Vec::new(),
    }
}

fn send_kind(name: &str, descriptor: &str) -> Option<IntentKind> {
    if !descriptor.contains("Landroid/content/Intent;") {
        return None;
    }
    match name {
        "startActivity" | "startActivityForResult" => Some(IntentKind::Activity),
        "sendBroadcast" | "sendOrderedBroadcast" => Some(IntentKind::Receiver),
        "startService" | "startForegroundService" => Some(IntentKind::StartedService),
        "bindService" => Some(IntentKind::BoundService),
        _ => None,
    }
}

fn is_local_broadcast_manager(cls: &str) -> bool {
    matches!(
        cls,
        "Landroidx/localbroadcastmanager/content/LocalBroadcastManager;"
            | "Landroid/support/v4/content/LocalBroadcastManager;"
    )
}

fn descriptor_to_dotted(descriptor: &str) -> Option<String> {
    descriptor
        .strip_prefix('L')?
        .strip_suffix(';')
        .map(|s| s.replace('/', "."))
}

fn emit_extras_assigns(facts: &mut IndexFacts, _ids: &IdMap, stats: &mut AndroidIntentStats) {
    let actuals: BTreeMap<_, _> = facts
        .actual_param
        .iter()
        .map(|(site, idx, vertex)| ((*site, *idx), vertex.clone()))
        .collect();
    let constants: BTreeMap<_, _> = facts
        .const_str_assign
        .iter()
        .map(|(site, vertex, value)| ((*site, vertex.clone()), *value))
        .collect();
    let mut assigns = BTreeSet::new();
    let mut paths = BTreeSet::new();

    for (site, cls, name, descriptor) in &facts.android_call_site {
        let sig = JavaSig {
            class: cls.as_ref(),
            name: name.as_ref(),
            descriptor: descriptor.as_ref(),
        };
        let InsnSiteId { insn_id, .. } = InsnSiteId::try_from(*site).unwrap();
        for (dst_idx, dst_path, src_idx, src_path) in api_rows(sig) {
            let dst = observed_api_receiver_vertex(sig, dst_idx).unwrap_or_else(|| {
                actuals
                    .get(&(*site, dst_idx))
                    .cloned()
                    .unwrap_or_else(|| call_arg_vertex(insn_id, dst_idx))
            });
            let src = observed_api_receiver_vertex(sig, src_idx).unwrap_or_else(|| {
                actuals
                    .get(&(*site, src_idx))
                    .cloned()
                    .unwrap_or_else(|| call_arg_vertex(insn_id, src_idx))
            });
            if !dst_path.is_empty() {
                paths.insert((dst_path,));
            }
            if !src_path.is_empty() {
                paths.insert((src_path,));
            }
            assigns.insert((
                *site,
                FlowVertex(dst.0, dst.1.concat(&dst_path)),
                FlowVertex(src.0, src.1.concat(&src_path)),
            ));
        }
        let Some(op) = extras_op(sig) else {
            continue;
        };
        let recv = actuals
            .get(&(*site, FormalIndex::new(0)))
            .cloned()
            .unwrap_or_else(|| call_arg_vertex(insn_id, FormalIndex::new(0)));
        let key_arg = FormalIndex::new(1);
        let value_arg = match op.kind {
            ExtrasKind::Put => FormalIndex::new(2),
            ExtrasKind::Get => RETURN_INDEX.into(),
        };
        let value = actuals
            .get(&(*site, value_arg))
            .cloned()
            .unwrap_or_else(|| call_arg_vertex(insn_id, value_arg));
        let key_vertex = call_arg_vertex(insn_id, key_arg);
        let key = constants.get(&(*site, key_vertex)).copied();
        let field_path = match (op.owner, key) {
            (ExtrasOwner::Intent, Some(key)) => Path::from_symbol_names(["<extras>", key.as_ref()]),
            (ExtrasOwner::Bundle, Some(key)) => Path::from_symbol_names([key.as_ref()]),
            (ExtrasOwner::Intent, None) => Path::from_symbol_names(["<extras>"]),
            (ExtrasOwner::Bundle, None) => Path::empty(),
        };
        if key.is_some() {
            stats.keyed_extra_sites += 1;
        } else {
            stats.lumped_extra_sites += 1;
        }
        if !field_path.is_empty() {
            paths.insert((field_path,));
        }
        let rooted = FlowVertex(recv.0, recv.1.concat(&field_path));
        let row = match op.kind {
            ExtrasKind::Put => (*site, rooted, value.clone()),
            ExtrasKind::Get => (*site, value.clone(), rooted),
        };
        assigns.insert(row);
        if matches!(op.kind, ExtrasKind::Get) && matches!(op.owner, ExtrasOwner::Intent) {
            let lumped = FlowVertex(
                recv.0,
                recv.1.concat(&Path::from_symbol_names(["<extras>"])),
            );
            paths.insert((Path::from_symbol_names(["<extras>"]),));
            assigns.insert((*site, value.clone(), lumped));
        }
        if matches!(op.kind, ExtrasKind::Put)
            && matches!(op.owner, ExtrasOwner::Intent)
            && key.is_some()
        {
            let lumped = FlowVertex(
                recv.0,
                recv.1.concat(&Path::from_symbol_names(["<extras>"])),
            );
            paths.insert((Path::from_symbol_names(["<extras>"]),));
            assigns.insert((*site, lumped, value));
        }
    }

    facts.assign.extend(assigns);
    facts.paths.extend(paths);
}

fn call_arg_vertex(insn_id: InsnId, formal: FormalIndex) -> FlowVertex {
    FlowVertex(call_arg_var(insn_id, formal), Path::empty())
}

fn observed_api_receiver_vertex(sig: JavaSig<'_>, formal: FormalIndex) -> Option<FlowVertex> {
    if matches!(
        (sig.name, sig.descriptor, *formal),
        ("getIntent", "()Landroid/content/Intent;", 0)
            | ("setIntent", "(Landroid/content/Intent;)V", 0)
    ) {
        Some(FlowVertex(
            FlowVariable::formal_index(FormalIndex::new(0)),
            Path::empty(),
        ))
    } else {
        None
    }
}

fn call_arg_var(insn_id: InsnId, formal: FormalIndex) -> FlowVariable {
    let packed = crate::facts::PackedCallArg::try_from_parts(insn_id, formal).unwrap();
    FlowVariable::call_arg_packed(packed)
}

#[derive(Debug, Clone, Copy)]
struct JavaSig<'a> {
    class: &'a str,
    name: &'a str,
    descriptor: &'a str,
}

impl<'a> JavaSig<'a> {
    fn parse(raw: &'a str) -> Option<Self> {
        let (class, rest) = raw.split_once("->")?;
        let args = rest.find('(')?;
        Some(Self {
            class,
            name: &rest[..args],
            descriptor: &rest[args..],
        })
    }
}

fn is_intent_api(sig: JavaSig<'_>) -> bool {
    matches!(sig.class, INTENT | BUNDLE | COMPONENT_NAME)
        || matches!(
            (sig.name, sig.descriptor),
            ("getIntent", "()Landroid/content/Intent;")
                | ("setIntent", "(Landroid/content/Intent;)V")
        )
}

fn is_send_api(sig: JavaSig<'_>) -> bool {
    matches!(
        sig.name,
        "startActivity"
            | "startActivityForResult"
            | "startService"
            | "startForegroundService"
            | "sendBroadcast"
            | "sendOrderedBroadcast"
            | "bindService"
    ) && sig.descriptor.contains("Landroid/content/Intent;")
}

fn api_rows(sig: JavaSig<'_>) -> Vec<(FormalIndex, Path, FormalIndex, Path)> {
    let mut rows = Vec::new();
    if sig.class == INTENT {
        match (sig.name, sig.descriptor) {
            ("<init>", "(Ljava/lang/String;)V") => {
                rows.push((idx(0), path("<action>"), idx(1), Path::empty()))
            }
            ("<init>", "(Ljava/lang/String;Landroid/net/Uri;)V") => {
                rows.push((idx(0), path("<action>"), idx(1), Path::empty()));
                rows.push((idx(0), path("<data>"), idx(2), Path::empty()));
            }
            ("<init>", "(Landroid/content/Context;Ljava/lang/Class;)V") => {
                rows.push((idx(0), path("<component>"), idx(2), Path::empty()))
            }
            ("<init>", "(Landroid/content/Intent;)V") => {
                rows.push((idx(0), Path::empty(), idx(1), Path::empty()))
            }
            ("setAction", _) => {
                rows.push((idx(0), path("<action>"), idx(1), Path::empty()));
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("getAction", "()Ljava/lang/String;") => {
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), path("<action>")))
            }
            ("setData", _) => {
                rows.push((idx(0), path("<data>"), idx(1), Path::empty()));
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("setType", _) => {
                rows.push((idx(0), path("<type>"), idx(1), Path::empty()));
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("setDataAndType", _) => {
                rows.push((idx(0), path("<data>"), idx(1), Path::empty()));
                rows.push((idx(0), path("<type>"), idx(2), Path::empty()));
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("getData", "()Landroid/net/Uri;") => {
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), path("<data>")))
            }
            ("setClass", _) | ("setClassName", _) | ("setComponent", _) => {
                if let Some(component_arg) = component_name_arg(sig) {
                    rows.push((idx(0), path("<component>"), component_arg, Path::empty()));
                }
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("getExtras", "()Landroid/os/Bundle;") => {
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), path("<extras>")))
            }
            ("putExtras", _) => {
                rows.push((idx(0), path("<extras>"), idx(1), Path::empty()));
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("putExtra", _) => {
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()));
            }
            ("createChooser", _) => {
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()))
            }
            (name, _) if name.starts_with("set") || name.starts_with("add") => {
                rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), Path::empty()))
            }
            _ => {}
        }
    } else if sig.class == COMPONENT_NAME {
        match sig.descriptor {
            "(Landroid/content/Context;Ljava/lang/String;)V"
            | "(Ljava/lang/String;Ljava/lang/String;)V" => {
                rows.push((idx(0), Path::empty(), idx(2), Path::empty()))
            }
            _ => {}
        }
    } else if matches!(
        (sig.name, sig.descriptor),
        ("getIntent", "()Landroid/content/Intent;") | ("setIntent", "(Landroid/content/Intent;)V")
    ) {
        if sig.name == "getIntent" {
            rows.push((RETURN_INDEX.into(), Path::empty(), idx(0), path("<intent>")));
        } else {
            rows.push((idx(0), path("<intent>"), idx(1), Path::empty()));
        }
    }
    rows
}

fn component_name_arg(sig: JavaSig<'_>) -> Option<FormalIndex> {
    match sig.descriptor {
        "(Landroid/content/Context;Ljava/lang/Class;)Landroid/content/Intent;" => Some(idx(2)),
        "(Landroid/content/ComponentName;)Landroid/content/Intent;" => Some(idx(1)),
        "(Landroid/content/Context;Ljava/lang/String;)Landroid/content/Intent;" => Some(idx(2)),
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;" => Some(idx(2)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct ExtrasOp {
    owner: ExtrasOwner,
    kind: ExtrasKind,
}

#[derive(Debug, Clone, Copy)]
enum ExtrasOwner {
    Intent,
    Bundle,
}

#[derive(Debug, Clone, Copy)]
enum ExtrasKind {
    Put,
    Get,
}

fn extras_op(sig: JavaSig<'_>) -> Option<ExtrasOp> {
    if sig.class == INTENT {
        if sig.name == "putExtra" {
            Some(ExtrasOp {
                owner: ExtrasOwner::Intent,
                kind: ExtrasKind::Put,
            })
        } else if sig.name.starts_with("get") && sig.name.ends_with("Extra") {
            Some(ExtrasOp {
                owner: ExtrasOwner::Intent,
                kind: ExtrasKind::Get,
            })
        } else {
            None
        }
    } else if sig.class == BUNDLE {
        if sig.name.starts_with("put") {
            Some(ExtrasOp {
                owner: ExtrasOwner::Bundle,
                kind: ExtrasKind::Put,
            })
        } else if sig.name.starts_with("get") {
            Some(ExtrasOp {
                owner: ExtrasOwner::Bundle,
                kind: ExtrasKind::Get,
            })
        } else {
            None
        }
    } else {
        None
    }
}

fn idx(value: i16) -> FormalIndex {
    value.into()
}

fn path(name: &str) -> Path {
    Path::from_symbol_names([name])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Function, PackedInsnSiteId, Str};

    #[test]
    fn emits_static_intent_api_summary_rows() {
        let mut ids = IdMap::new();
        let set_action = ids.get_or_add_function(Function(
            "Landroid/content/Intent;->setAction(Ljava/lang/String;)Landroid/content/Intent;"
                .into(),
        ));
        let mut facts = IndexFacts::default();

        let stats = emit_phase2_facts(&mut facts, &ids);

        assert_eq!(stats.api_functions, 1);
        assert!(
            facts
                .summary
                .iter()
                .any(|(func, dst, dst_path, src, src_path)| {
                    *func == set_action
                        && **dst == 0
                        && *dst_path == Path::from_symbol_names(["<action>"])
                        && **src == 1
                        && src_path.is_empty()
                })
        );
    }

    #[test]
    fn literal_put_extra_emits_keyed_assign_rooted_at_receiver() {
        let mut ids = IdMap::new();
        let caller = ids.get_or_add_function(Function("caller".into()));
        let put_extra = ids.get_or_add_function(Function(
            "Landroid/content/Intent;->putExtra(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;".into(),
        ));
        let site = PackedInsnSiteId::try_from_parts(caller, InsnId::new(10)).unwrap();
        let recv = FlowVariable::local(Str::from("intent"));
        let value = FlowVariable::local(Str::from("value"));
        let key_arg = call_arg_vertex(InsnId::new(10), FormalIndex::new(1));
        let mut facts = IndexFacts {
            call: vec![(site, put_extra)],
            android_call_site: vec![(
                site,
                Str::from("Landroid/content/Intent;"),
                Str::from("putExtra"),
                Str::from("(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;"),
            )],
            actual_param: vec![
                (site, FormalIndex::new(0), FlowVertex(recv, Path::empty())),
                (site, FormalIndex::new(2), FlowVertex(value, Path::empty())),
            ],
            const_str_assign: vec![(site, key_arg, Str::from("secret"))],
            ..Default::default()
        };

        let stats = emit_phase2_facts(&mut facts, &ids);

        assert_eq!(stats.keyed_extra_sites, 1);
        assert!(facts.assign.iter().any(|(assign_site, dst, src)| {
            *assign_site == site
                && *dst == FlowVertex(recv, Path::from_symbol_names(["<extras>", "secret"]))
                && *src == FlowVertex(value, Path::empty())
        }));
    }

    #[test]
    fn bundle_keyed_extra_lives_at_bundle_root() {
        let mut ids = IdMap::new();
        let caller = ids.get_or_add_function(Function("caller".into()));
        let put_string = ids.get_or_add_function(Function(
            "Landroid/os/Bundle;->putString(Ljava/lang/String;Ljava/lang/String;)V".into(),
        ));
        let site = PackedInsnSiteId::try_from_parts(caller, InsnId::new(11)).unwrap();
        let bundle = FlowVariable::local(Str::from("bundle"));
        let value = FlowVariable::local(Str::from("value"));
        let key_arg = call_arg_vertex(InsnId::new(11), FormalIndex::new(1));
        let mut facts = IndexFacts {
            call: vec![(site, put_string)],
            android_call_site: vec![(
                site,
                Str::from("Landroid/os/Bundle;"),
                Str::from("putString"),
                Str::from("(Ljava/lang/String;Ljava/lang/String;)V"),
            )],
            actual_param: vec![
                (site, FormalIndex::new(0), FlowVertex(bundle, Path::empty())),
                (site, FormalIndex::new(2), FlowVertex(value, Path::empty())),
            ],
            const_str_assign: vec![(site, key_arg, Str::from("secret"))],
            ..Default::default()
        };

        let stats = emit_phase2_facts(&mut facts, &ids);

        assert_eq!(stats.keyed_extra_sites, 1);
        assert!(facts.assign.iter().any(|(assign_site, dst, src)| {
            *assign_site == site
                && *dst == FlowVertex(bundle, Path::from_symbol_names(["secret"]))
                && *src == FlowVertex(value, Path::empty())
        }));
        assert!(
            !facts
                .assign
                .iter()
                .any(|(_, dst, _)| dst.1 == Path::from_symbol_names(["<extras>", "secret"]))
        );
    }

    #[test]
    fn observed_send_sites_emit_intent_send_and_delivery() {
        let mut ids = IdMap::new();
        let caller = ids.get_or_add_function(Function("caller".into()));
        let site = PackedInsnSiteId::try_from_parts(caller, InsnId::new(10)).unwrap();
        let intent = FlowVariable::local(Str::from("intent"));
        let mut facts = IndexFacts {
            android_call_site: vec![(
                site,
                Str::from("Landroid/app/Activity;"),
                Str::from("startActivity"),
                Str::from("(Landroid/content/Intent;)V"),
            )],
            actual_param: vec![(site, FormalIndex::new(1), FlowVertex(intent, Path::empty()))],
            ..Default::default()
        };
        let mut source_info = IndexSourceInfo::default();
        source_info.sites = ids;

        let mut stats = AndroidIntentStats::default();
        emit_send_sites(&mut facts, &mut source_info, &mut stats);

        assert_eq!(facts.intent_send.len(), 1);
        assert_eq!(facts.intent_send[0].0, caller);
        assert_eq!(facts.intent_send[0].2, InsnId::new(10));
        assert_eq!(facts.intent_send[0].4, IntentKind::Activity);
        assert_eq!(facts.assign.len(), 2);
        assert!(facts.intent_frame.contains(&(caller,)));
    }

    #[test]
    fn delivery_positions_follow_send_kind() {
        let mut ids = IdMap::new();
        let caller = ids.get_or_add_function(Function("caller".into()));
        let intent = FlowVariable::local(Str::from("intent"));
        let cases = [
            (
                "sendBroadcast",
                "(Landroid/content/Intent;)V",
                IntentKind::Receiver,
                2,
            ),
            (
                "startService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;",
                IntentKind::StartedService,
                1,
            ),
            (
                "bindService",
                "(Landroid/content/Intent;Landroid/content/ServiceConnection;I)Z",
                IntentKind::BoundService,
                1,
            ),
        ];

        for (i, (name, descriptor, kind, delivered_formal)) in cases.into_iter().enumerate() {
            let insn = InsnId::new(20 + i as u64);
            let site = PackedInsnSiteId::try_from_parts(caller, insn).unwrap();
            let mut facts = IndexFacts {
                android_call_site: vec![(
                    site,
                    Str::from("Landroid/content/Context;"),
                    Str::from(name),
                    Str::from(descriptor),
                )],
                actual_param: vec![(site, FormalIndex::new(1), FlowVertex(intent, Path::empty()))],
                ..Default::default()
            };
            let mut source_info = IndexSourceInfo::default();
            source_info.sites = ids.clone();

            let mut stats = AndroidIntentStats::default();
            emit_send_sites(&mut facts, &mut source_info, &mut stats);

            assert_eq!(facts.intent_send.len(), 1);
            assert_eq!(facts.intent_send[0].4, kind);
            let delivered = facts.assign.iter().any(|(_, dst, src)| {
                src.1.is_empty()
                    && dst.1.is_empty()
                    && dst
                        .0
                        .as_call_arg()
                        .and_then(|packed| crate::facts::CallArgId::try_from(packed).ok())
                        .is_some_and(|arg| arg.formal == delivered_formal)
            });
            assert!(
                delivered,
                "{name} did not deliver at formal {delivered_formal}"
            );
        }
    }

    #[test]
    fn local_broadcast_manager_is_not_a_manifest_send() {
        let mut ids = IdMap::new();
        let caller = ids.get_or_add_function(Function("caller".into()));
        let site = PackedInsnSiteId::try_from_parts(caller, InsnId::new(10)).unwrap();
        let mut facts = IndexFacts {
            android_call_site: vec![(
                site,
                Str::from("Landroidx/localbroadcastmanager/content/LocalBroadcastManager;"),
                Str::from("sendBroadcast"),
                Str::from("(Landroid/content/Intent;)Z"),
            )],
            ..Default::default()
        };
        let mut source_info = IndexSourceInfo::default();
        source_info.sites = ids;

        let mut stats = AndroidIntentStats::default();
        emit_send_sites(&mut facts, &mut source_info, &mut stats);

        assert!(facts.intent_send.is_empty());
        assert!(facts.assign.is_empty());
    }

    #[test]
    fn entry_methods_walk_to_app_base_class() {
        let mut ids = IdMap::new();
        let base_on_create = ids.get_or_add_function(Function(
            "Lcom/example/BaseActivity;->onCreate(Landroid/os/Bundle;)V".into(),
        ));
        let observer = AndroidIntentObserver {
            parents: BTreeMap::from([(
                "Lcom/example/MainActivity;".to_string(),
                vec!["Lcom/example/BaseActivity;".to_string()],
            )]),
        };

        let entries = entry_methods(
            &ids,
            &observer,
            "Lcom/example/MainActivity;",
            IntentKind::Activity,
        );

        assert!(entries.contains(&base_on_create));
    }

    #[test]
    fn service_send_kinds_are_separate() {
        assert_eq!(
            send_kind(
                "startService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;"
            ),
            Some(IntentKind::StartedService)
        );
        assert_eq!(
            send_kind(
                "startForegroundService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;"
            ),
            Some(IntentKind::StartedService)
        );
        assert_eq!(
            send_kind(
                "bindService",
                "(Landroid/content/Intent;Landroid/content/ServiceConnection;I)Z"
            ),
            Some(IntentKind::BoundService)
        );
    }

    #[test]
    fn activity_filters_require_default_category() {
        let manifest = crate::languages::android_manifest::parse_manifest(
            br#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
                <application>
                    <activity android:name=".MainActivity">
                        <intent-filter>
                            <action android:name="android.intent.action.MAIN" />
                            <category android:name="android.intent.category.LAUNCHER" />
                        </intent-filter>
                        <intent-filter>
                            <action android:name="com.example.SEND" />
                            <category android:name="android.intent.category.DEFAULT" />
                        </intent-filter>
                    </activity>
                    <receiver android:name=".Receiver">
                        <intent-filter>
                            <action android:name="com.example.RECEIVE" />
                        </intent-filter>
                    </receiver>
                </application>
            </manifest>"#,
        )
        .unwrap();
        let activity = manifest
            .components()
            .into_iter()
            .find(|component| component.tag == "activity")
            .unwrap();
        let receiver = manifest
            .components()
            .into_iter()
            .find(|component| component.tag == "receiver")
            .unwrap();

        let activity_actions: Vec<_> =
            filters_for_component_node(&manifest, activity.node_id, IntentKind::Activity)
                .into_iter()
                .flat_map(|filter| filter.actions)
                .collect();
        let receiver_actions: Vec<_> =
            filters_for_component_node(&manifest, receiver.node_id, IntentKind::Receiver)
                .into_iter()
                .flat_map(|filter| filter.actions)
                .collect();

        assert_eq!(activity_actions, vec!["com.example.SEND".to_string()]);
        assert_eq!(receiver_actions, vec!["com.example.RECEIVE".to_string()]);
    }
}
