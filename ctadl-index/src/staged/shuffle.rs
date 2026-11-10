/*!
A seeded shuffle of every fact vector, so that tests can check that an index does not depend on
the order of its facts.
*/

use crate::index_engine::IndexFacts;

/// Reorders every vector of `facts` by a Fisher–Yates shuffle. The same `seed` gives the same
/// order on every platform.
pub fn shuffle_facts(facts: &mut IndexFacts, seed: u64) {
    let mut state = seed ^ 0x9e37_79b9;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    };
    // Every field is named, so a new one is a compile error here until it is shuffled too.
    let IndexFacts {
        formal_param,
        actual_param,
        call,
        assign,
        const_str_assign,
        android_call_site,
        callee_info,
        summary,
        paths,
        intent_frame,
        intent_send,
        intent_filter,
        intent_component,
        external_function,
        globals,
        global_flow,
    } = facts;
    shuffle(formal_param, &mut next);
    shuffle(actual_param, &mut next);
    shuffle(call, &mut next);
    shuffle(assign, &mut next);
    shuffle(const_str_assign, &mut next);
    shuffle(android_call_site, &mut next);
    shuffle(callee_info, &mut next);
    shuffle(summary, &mut next);
    shuffle(paths, &mut next);
    shuffle(intent_frame, &mut next);
    shuffle(intent_send, &mut next);
    shuffle(intent_filter, &mut next);
    shuffle(intent_component, &mut next);
    shuffle(external_function, &mut next);
    shuffle(globals, &mut next);
    shuffle(global_flow, &mut next);
}

fn shuffle<T>(v: &mut [T], next: &mut impl FnMut() -> u64) {
    for i in (1..v.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::FunctionId;

    fn facts() -> IndexFacts {
        IndexFacts {
            external_function: (0..50).map(|i| (FunctionId::new(i),)).collect(),
            ..Default::default()
        }
    }

    /// A seed gives one permutation, and another seed another.
    #[test]
    fn shuffle_is_a_seeded_permutation() {
        let mut a = facts();
        let mut b = facts();
        shuffle_facts(&mut a, 1);
        shuffle_facts(&mut b, 1);
        assert_eq!(a.external_function, b.external_function);
        assert_ne!(a.external_function, facts().external_function);
        let mut c = facts();
        shuffle_facts(&mut c, 2);
        assert_ne!(a.external_function, c.external_function);
        c.external_function.sort();
        assert_eq!(c.external_function, facts().external_function);
    }
}
