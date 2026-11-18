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

            // This model finds calls to `AsyncTask.execute` and routes them to `doInBackground`
            synth_call(site_id, do_in_bg_f) <--
                // Find calls to execute that return tasks
                call(site_id, func_id),
                if let Some(execute_method) = idmap.get_function(*func_id),
                if let Some((cls_name, _)) = execute_method.split_once(".execute:"),
                if execute_method.ends_with("Landroid/os/AsyncTask;"),
                // Find the doInBackground method of the same class
                let execute_cls = JavaClass(cls_name.into()),
                method(execute_cls, simple_name, sig, do_in_bg),
                if **simple_name == "doInBackground",
                // Synth a call to doInBackground
                if let Some(do_in_bg_f) = idmap.get_function_id((**do_in_bg).clone().into());
        };
        log::trace!("synth_call: {:?}", result.synth_call);
        facts.call.extend(result.synth_call);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Function, Str};

    fn java_method(
        cls: &str,
        name: &str,
        sig: &str,
    ) -> (JavaClass, JavaSimpleName, JavaSignature, JavaMethod) {
        (
            JavaClass(cls.into()),
            JavaSimpleName(name.into()),
            JavaSignature(sig.into()),
            JavaMethod(format!("{cls}->{name}{sig}").into()),
        )
    }

    #[test]
    fn async_task_execute_reaches_only_its_own_class_do_in_background() {
        let sig = "([Ljava/lang/Object;)Ljava/lang/Object;";
        let mine = java_method("Lcom/example/MyTask;", "doInBackground", sig);
        let other = java_method("Lcom/example/OtherTask;", "doInBackground", sig);
        let vmt = VirtualMethodTable::Java {
            methods: vec![mine.clone(), other.clone()],
            hierarchy: Default::default(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        };

        let mut idmap = IdMap::new();
        let mut add = |name: &str| idmap.get_or_add_function(Function::from(Str::from(name)));
        let execute =
            add("Lcom/example/MyTask;.execute:([Ljava/lang/Object;)Landroid/os/AsyncTask;");
        let mine_f = add(&mine.3);
        add(&other.3);

        let site = PackedInsnSiteId([1; 8]);
        let mut facts = IndexFacts {
            call: vec![(site, execute)],
            ..Default::default()
        };
        load_models(&vmt, &mut facts, &idmap);

        let mut calls = facts.call.clone();
        calls.sort();
        assert_eq!(calls, vec![(site, execute), (site, mine_f)]);
    }
}
