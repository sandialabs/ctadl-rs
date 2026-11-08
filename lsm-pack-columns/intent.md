# Intent: pack LSM columns - DO-NOT-MERGE

Make the `lsm` provider store each column in its natural width instead of a
whole `u64` slot (`lsm_col!` in `lsm/src/ascent_provider.rs`), so narrow columns
(`FunctionId`, `FormalIndex`, and 4-byte handles from `intern-everything`) take
less space in the relations the lsm engine holds.
