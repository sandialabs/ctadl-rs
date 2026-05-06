use ctadl_ir::call::{JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable};

use crate::facts::{FunctionId, IdMap, PackedInsnSiteId};
use crate::index_engine::{FunctionSummary, IndexFacts};

/// This hook is called after codegen so that arbitrary models, specifically those that aren't
/// easily expressible at the language level, can be generated. This includes models that are based
/// on finding call sites and models that bridge languages.
pub fn load_models(vmt: &VirtualMethodTable, facts: &mut IndexFacts, idmap: &IdMap) {
    if let VirtualMethodTable::Java { methods, .. } = vmt {
        let result = ascent::ascent_run! {
            relation method(JavaClass, JavaSimpleName, JavaSignature, JavaMethod) = methods.clone();
            relation call(PackedInsnSiteId, FunctionId) = facts.call.clone();
            relation synth_call(PackedInsnSiteId, FunctionId);
            relation summary(FunctionSummary);

            // This models finds calls to `AsyncTask.execute` and routes them to `doInBackground`
            synth_call(site_id, do_in_bg_f) <--
                // Find calls to execute that return tasks
                call(site_id, func_id),
                if let Some(execute_method) = idmap.get_function(*func_id),
                if execute_method.contains(".execute:"),
                if execute_method.ends_with("Landroid/os/AsyncTask;"),
                // Find the doInBackground method of the same class
                method(execute_cls, simple_name, sig, do_in_bg),
                if **simple_name == "doInBackground",
                // Synth a call to doInBackground
                if let Some(do_in_bg_f) = idmap.get_function_id((**do_in_bg).clone().into());
        };
        log::trace!("synth_call: {:?}", &result.synth_call);
        facts.call.extend(result.synth_call);
    }
}
