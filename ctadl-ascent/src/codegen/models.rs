/*! Codegen for models

Take encoded models and codegen them into the index facts
*/

use internment::ArcIntern;

use crate::codegen::{GLOBALS_INDEX, RETURN_INDEX};
use crate::facts;
use crate::index_engine::IndexFacts;
use crate::index_engine::source_info::IndexSourceInfo;
use crate::models::{FormalIndexTypeTag, ModelsBatch, SummaryBatch};

/// Take a bunch of models and codegen them into the facts.
#[inline]
pub fn codegen_models(
    models: ModelsBatch,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    codegen_summary(models.summary, facts, source_info);
}

/// Take a batch of summaries and codegen them into the facts.
pub fn codegen_summary(
    batch: SummaryBatch,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    let ap_map = batch.aps.build_ap_map();
    let func_num_params = facts.compute_num_params();

    for record @ (func, dst_tag, dst_index, dst_ap, src_tag, src_index, src_ap) in
        batch.iter_summaries()
    {
        log::trace!("{:?}", record);
        use FormalIndexTypeTag::*;
        let dst_ap: facts::Path = ap_map[&dst_ap].iter().cloned().collect();
        let Some(func_id) = source_info
            .sites
            .get_function_id(facts::Function(ArcIntern::from(func)))
        else {
            // skip functions that don't occur in the facts
            continue;
        };
        let dst_index: Vec<facts::FormalIndex> = match dst_tag {
            Index => {
                let dst_index: i16 = dst_index.unwrap();
                vec![dst_index.into()]
            }
            Return => vec![RETURN_INDEX.into()],
            Global => vec![GLOBALS_INDEX.into()],
            AnyArgument => func_num_params
                .get(&func_id)
                .map(|n| (0..*n).map(|i| i.into()).collect())
                .unwrap_or_default(),
        };
        let src_ap: facts::Path = ap_map[&src_ap].iter().cloned().collect();
        let src_index: Vec<facts::FormalIndex> = match src_tag {
            Index => {
                let src_index: i16 = src_index.unwrap();
                vec![src_index.into()]
            }
            Return => vec![RETURN_INDEX.into()],
            Global => vec![GLOBALS_INDEX.into()],
            AnyArgument => func_num_params
                .get(&func_id)
                .map(|n| (0..*n).map(|i| i.into()).collect())
                .unwrap_or_default(),
        };

        log::trace!("dst_index: {}", dst_index.len());
        log::trace!("src_index: {}", src_index.len());
        // codegen
        for dst_index in &dst_index {
            // Ensure formal_param exists for the indices used in summaries
            facts.formal_param.push((
                func_id,
                facts::FlowVariable::Formal(*dst_index),
                facts::FormalType::ByRef,
            ));
            for src_index in &src_index {
                // Ensure formal_param exists for the indices used in summaries
                facts.formal_param.push((
                    func_id,
                    facts::FlowVariable::Formal(*src_index),
                    facts::FormalType::ByRef,
                ));
                if dst_index == src_index {
                    continue;
                }
                facts.summary.push((
                    func_id,
                    *dst_index,
                    dst_ap.clone(),
                    *src_index,
                    src_ap.clone(),
                ));
            }
        }
    }
}
