//! Example demonstrating access-path field accesses and how they are spelled as text.
//!
//! Every string printed here is in the one canonical access-path grammar
//! (`ctadl_ir::mir::path_syntax`), the same grammar the fact store, model-generator ports, and
//! the flowy test DSL use. So anything this prints can be pasted into a model port and mean
//! exactly what it printed.

use ctadl_ir::mir::{
    AccessPath, FieldAccess, FieldAccesses, FieldPath, Locals, Offset, PathSegment, VariableRef,
    path_syntax,
};

fn main() {
    // Offsets are decimal, in brackets. (They used to print as signed hex, `0x2a`, which no
    // parser in the tree accepted.)
    let offset = Offset(42);
    println!("Offset:          {offset}"); // 42
    println!("Negative offset: {}", Offset(-40)); // -40

    // An access path is offset-only: symbolic fields are reached through a load/store.
    let offset_path = FieldAccesses::with_offset(100);
    println!("Offset path:     {offset_path}"); // .[100]

    let offsets = FieldAccesses::with_offsets([50, 75]);
    println!("Two offsets:     {offsets}"); // .[50].[75]

    let mut locals = Locals::default();
    let var = VariableRef::new_local_idx(locals.get_or_intern("obj"));
    let access_path = AccessPath {
        variable_ref: var,
        path: offsets.clone(),
    };
    println!("Access path:     {access_path}"); // %L0.[50].[75]

    // A field path is the symbolic half, written by a load or a store.
    println!("Field path:      {}", FieldPath::symbol("deref")); // .deref

    // Individual segments. `PathSegment` is the mixed form -- the element of the analysis-level
    // path -- and is what parses and prints.
    println!("Symbol segment:  {}", PathSegment::symbol("name")); // name
    println!("Offset segment:  {}", PathSegment::offset(123)); // [123]
    println!("Field access:    {}", FieldAccess::Offset(Offset(123))); // [123]

    // Escaping is what keeps the grammar unambiguous. A field name that *begins* with '[' would
    // otherwise read back as an offset, so it is escaped -- which is how the frontends'
    // synthetic array-element fields survive a round trip through the fact store.
    println!();
    for name in ["[]", "[_elem_]", "[3]", "a.b", r"a\b", "a[3]"] {
        let seg = PathSegment::symbol(name);
        let text = seg.to_string();
        let back = path_syntax::parse_segment(&text).unwrap();
        println!("Symbol({name:>9?}) prints as {text:>12}  and parses back to {back:?}");
        assert_eq!(back, seg);
    }

    // Note the pair that used to collide: both spelled `.[3]`, so the symbol lost.
    println!();
    println!(
        "Symbol(\"[3]\") -> {:?}   vs   Offset(3) -> {:?}",
        PathSegment::symbol("[3]").to_string(),
        PathSegment::offset(3).to_string()
    );
}
