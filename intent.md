# Intent: intern everything - DO-NOT-MERGE

Make the index engine's Datalog relations use less space by interning the key
data structures the rules use and replacing them with u32 handles.

Take inspiration from the interned `PathId`s in the `../ctadl-access-tree`
branch. Build on whichever interner design fits best, `access-tree`'s
`concurrent.rs` or this workspace's `immortal` crate, or an improvement on both.
