//! Ported from Test/Mono.Cecil.Tests/PropertyTests.cs and EventTests.cs
//! (read subsets). Skipped: UnattachedProperty/UnattachedEvent (construct
//! definitions).

use super::{Test, read_fixture};
use cil_reader::Module;

// Test/Mono.Cecil.Tests/PropertyTests.cs:11 (AbstractMethod)
fn property_abstract_method() {
    let data = read_fixture("Properties.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Foo").unwrap().unwrap();
    assert!(ty.has_properties().unwrap());
    let properties = ty.properties().unwrap();
    assert_eq!(properties.len(), 3);

    let property = &properties[0];
    assert_eq!(property.name().unwrap(), "Bar");
    let sig = property.signature().unwrap();
    assert_eq!(
        module
            .type_sig_full_name(&sig.property_type, Default::default())
            .unwrap(),
        "System.Int32"
    );
    let getter = property.get_method().unwrap().expect("getter");
    assert!(getter.is_getter().unwrap());
    assert!(property.set_method().unwrap().is_none());

    let property = &properties[1];
    assert_eq!(property.name().unwrap(), "Baz");
    let sig = property.signature().unwrap();
    assert_eq!(
        module
            .type_sig_full_name(&sig.property_type, Default::default())
            .unwrap(),
        "System.String"
    );
    assert!(property.get_method().unwrap().unwrap().is_getter().unwrap());
    assert!(property.set_method().unwrap().unwrap().is_setter().unwrap());

    let property = &properties[2];
    assert_eq!(property.name().unwrap(), "Gazonk");
    assert!(property.get_method().unwrap().is_none());
    assert!(property.set_method().unwrap().unwrap().is_setter().unwrap());
}

// Test/Mono.Cecil.Tests/PropertyTests.cs:59 (OtherMethod)
fn property_other_method() {
    let data = read_fixture("others.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Others").unwrap().unwrap();
    assert!(ty.has_properties().unwrap());
    let properties = ty.properties().unwrap();
    assert_eq!(properties.len(), 1);

    let property = &properties[0];
    assert_eq!(property.name().unwrap(), "Context");
    let sig = property.signature().unwrap();
    assert_eq!(
        module
            .type_sig_full_name(&sig.property_type, Default::default())
            .unwrap(),
        "System.String"
    );

    let others = property.other_methods().unwrap();
    assert_eq!(others.len(), 2);
    assert_eq!(others[0].name().unwrap(), "let_Context");
    assert_eq!(others[1].name().unwrap(), "bet_Context");
}

// Test/Mono.Cecil.Tests/PropertyTests.cs:90 (SetOnlyIndexer)
fn set_only_indexer() {
    let data = read_fixture("Properties.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Bar").unwrap().unwrap();
    let indexer = ty
        .properties()
        .unwrap()
        .into_iter()
        .find(|p| p.name().unwrap() == "Item")
        .unwrap();
    let parameters = indexer.parameters().unwrap();
    assert_eq!(parameters.len(), 2);
    assert_eq!(
        module
            .type_sig_full_name(&parameters[0], Default::default())
            .unwrap(),
        "System.Int32"
    );
    assert_eq!(
        module
            .type_sig_full_name(&parameters[1], Default::default())
            .unwrap(),
        "System.String"
    );
}

// Test/Mono.Cecil.Tests/PropertyTests.cs:104 (ReadSemanticsFirst)
fn read_semantics_first() {
    let data = read_fixture("Properties.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Baz").unwrap().unwrap();
    let setter = ty
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == "set_Bingo")
        .unwrap();
    assert!(setter.is_setter().unwrap());

    let property = ty
        .properties()
        .unwrap()
        .into_iter()
        .find(|p| p.name().unwrap() == "Bingo")
        .unwrap();
    assert_eq!(property.set_method().unwrap().unwrap(), setter);
    assert_eq!(
        property.get_method().unwrap().unwrap().name().unwrap(),
        "get_Bingo"
    );
}

// Test/Mono.Cecil.Tests/EventTests.cs:14 (AbstractMethod)
fn event_abstract_method() {
    let data = read_fixture("Events.cs.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Foo").unwrap().unwrap();
    assert!(ty.has_events().unwrap());
    let events = ty.events().unwrap();
    assert_eq!(events.len(), 1);

    let event = &events[0];
    assert_eq!(event.name().unwrap(), "Bar");
    assert_eq!(
        event.event_type().unwrap().unwrap().full_name().unwrap(),
        "Pan"
    );

    assert!(event.add_method().unwrap().unwrap().is_add_on().unwrap());
    assert!(
        event
            .remove_method()
            .unwrap()
            .unwrap()
            .is_remove_on()
            .unwrap()
    );
}

// Test/Mono.Cecil.Tests/EventTests.cs:39 (OtherMethod)
fn event_other_method() {
    let data = read_fixture("others.il.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("Others").unwrap().unwrap();
    assert!(ty.has_events().unwrap());
    let events = ty.events().unwrap();
    assert_eq!(events.len(), 1);

    let event = &events[0];
    assert_eq!(event.name().unwrap(), "Handler");
    assert_eq!(
        event.event_type().unwrap().unwrap().full_name().unwrap(),
        "System.EventHandler"
    );

    let others = event.other_methods().unwrap();
    assert_eq!(others.len(), 2);
    assert_eq!(others[0].name().unwrap(), "dang_Handler");
    assert_eq!(others[1].name().unwrap(), "fang_Handler");
}

pub(super) const TESTS: &[Test] = &[
    ("property_abstract_method", property_abstract_method),
    ("property_other_method", property_other_method),
    ("set_only_indexer", set_only_indexer),
    ("read_semantics_first", read_semantics_first),
    ("event_abstract_method", event_abstract_method),
    ("event_other_method", event_other_method),
];
