// Example demonstrating the new offset field access functionality
use ctadl_ir::mir::{AccessPath, FieldAccess, FieldAccesses, Offset, VariableRef};

fn main() {
    // Create a simple offset
    let offset = Offset(42);
    println!("Offset: {}", offset); // Output: Offset: 42

    // Create field accesses with offsets
    let offset_path = FieldAccesses::with_offset(100);
    println!("Offset path: {}", offset_path); // Output: Offset path: .[100]

    // Create mixed field accesses (symbols and offsets)
    let mixed_path = FieldAccesses::mixed(vec![Ok("field1"), Err(50), Ok("field2"), Err(75)]);
    println!("Mixed path: {}", mixed_path); // Output: Mixed path: .field1.[50].field2.[75]

    // Create an access path with mixed field accesses
    let var = VariableRef::new_local("obj".to_string());
    let access_path = AccessPath {
        variable_ref: var,
        path: mixed_path,
    };
    println!("Access path: {}", access_path); // Output: Access path: %obj.field1.[50].field2.[75]

    // Individual field access examples
    let symbol_access = FieldAccess::Symbol("name".into());
    let offset_access = FieldAccess::Offset(Offset(123));
    println!("Symbol access: {}", symbol_access); // Output: Symbol access: name
    println!("Offset access: {}", offset_access); // Output: Offset access: [123]
}
