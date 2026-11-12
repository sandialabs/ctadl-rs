/*!
The engine-free parts of a staged index run.

A staged run computes the index as a sequence of stages instead of one fixpoint over the whole
program, so that its peak memory is that of its largest stage:

1. Batches of functions, callees first. Each runs the index rules on its functions' facts and
   its callees' summaries, and writes its functions' `summary` and `assign` rows and its
   global-to-global edges.
2. The global frame: the union of every batch's global-to-global edges.
3. The intent rules, over the functions that send intents and the frame.

Every stage reads its inputs from, and writes its outputs to, the stage directory
([`store`]), so each can run in a fresh process and a killed run can resume. This module holds
the parts that need no engine: the stage directory, the files in it ([`files`]), the plan of
batches ([`plan`]) and a seeded shuffle of facts for tests ([`shuffle`]).
*/

pub mod files;
pub mod plan;
pub mod shuffle;
pub mod stats;
pub mod store;
