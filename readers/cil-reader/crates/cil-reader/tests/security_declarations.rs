//! Ported from Test/Mono.Cecil.Tests/SecurityDeclarationTests.cs (read
//! subset). Skipped: DefineSecurityDeclarationByBlob (writes a module).

mod testutil;

use cil_reader::Module;
use cil_reader::model::CaValue;
use cil_reader::model::custom_attributes::SecurityDeclarationValue;
use testutil::read_resource;

// SecurityAction values.
const DENY: u16 = 4;
const LINK_DEMAND: u16 = 6;

const PERMISSION_SET_20: &str = "<PermissionSet class=\"System.Security.PermissionSe\
t\"\r\nversion=\"1\">\r\n<IPermission class=\"System.Security.Permis\
sions.SecurityPermission, mscorlib, Version=2.0.0.0, Culture\
=neutral, PublicKeyToken=b77a5c561934e089\"\r\nversion=\"1\"\r\nFla\
gs=\"UnmanagedCode\"/>\r\n</PermissionSet>\r\n";

const PERMISSION_SET_11: &str = "<PermissionSet class=\"System.Security.PermissionSe\
t\"\r\nversion=\"1\">\r\n<IPermission class=\"System.Security.Permis\
sions.SecurityPermission, mscorlib, Version=1.0.0.0, Culture\
=neutral, PublicKeyToken=b77a5c561934e089\"\r\nversion=\"1\"\r\nFla\
gs=\"UnmanagedCode\"/>\r\n</PermissionSet>\r\n";

// Test/Mono.Cecil.Tests/SecurityDeclarationTests.cs:18 (XmlSecurityDeclaration)
#[test]
fn xml_security_declaration() {
    let data = read_resource!("decsec-xml.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("SubLibrary").unwrap().unwrap();

    let declarations = module.security_declarations_of(ty.token()).unwrap();
    assert_eq!(declarations.len(), 1);
    let declaration = &declarations[0];
    assert_eq!(declaration.action().unwrap(), DENY);

    // Cecil surfaces the legacy XML form as a synthesized
    // PermissionSetAttribute with an XML property; we surface the XML string.
    match declaration.value().unwrap() {
        SecurityDeclarationValue::Xml(xml) => assert_eq!(xml, PERMISSION_SET_20),
        other => panic!("expected xml declaration, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/SecurityDeclarationTests.cs:57 (XmlNet_1_1SecurityDeclaration)
#[test]
fn xml_net_1_1_security_declaration() {
    let data = read_resource!("decsec1-xml.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("SubLibrary").unwrap().unwrap();

    let declarations = module.security_declarations_of(ty.token()).unwrap();
    assert_eq!(declarations.len(), 1);
    let declaration = &declarations[0];
    assert_eq!(declaration.action().unwrap(), DENY);

    match declaration.value().unwrap() {
        SecurityDeclarationValue::Xml(xml) => assert_eq!(xml, PERMISSION_SET_11),
        other => panic!("expected xml declaration, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/SecurityDeclarationTests.cs:130 (SecurityDeclarationWithoutAttributes)
#[test]
fn security_declaration_without_attributes() {
    let data = read_resource!("empty-decsec-att.dll");
    let module = Module::read(&data).unwrap();
    let ty = module
        .get_type("TestSecurityAction.ModalUITypeEditor")
        .unwrap()
        .unwrap();
    let method = ty
        .methods()
        .unwrap()
        .into_iter()
        .find(|m| m.name().unwrap() == "GetEditStyle")
        .unwrap();

    let declarations = module.security_declarations_of(method.token()).unwrap();
    assert_eq!(declarations.len(), 1);
    let declaration = &declarations[0];
    assert_eq!(declaration.action().unwrap(), LINK_DEMAND);

    match declaration.value().unwrap() {
        SecurityDeclarationValue::Attributes(attributes) => {
            assert_eq!(attributes.len(), 1);
            assert_eq!(
                attributes[0].attribute_type,
                "System.Security.Permissions.SecurityPermissionAttribute"
            );
            assert!(attributes[0].fields.is_empty());
            assert!(attributes[0].properties.is_empty());
        }
        other => panic!("expected attribute declaration, got {other:?}"),
    }
}

// Test/Mono.Cecil.Tests/SecurityDeclarationTests.cs:152 (AttributeSecurityDeclaration)
#[test]
fn attribute_security_declaration() {
    let data = read_resource!("decsec-att.dll");
    let module = Module::read(&data).unwrap();
    let ty = module.get_type("SubLibrary").unwrap().unwrap();

    let declarations = module.security_declarations_of(ty.token()).unwrap();
    assert_eq!(declarations.len(), 1);
    let declaration = &declarations[0];
    assert_eq!(declaration.action().unwrap(), DENY);

    match declaration.value().unwrap() {
        SecurityDeclarationValue::Attributes(attributes) => {
            assert_eq!(attributes.len(), 1);
            let attribute = &attributes[0];
            assert_eq!(
                attribute.attribute_type,
                "System.Security.Permissions.SecurityPermissionAttribute"
            );
            assert_eq!(attribute.properties.len(), 1);
            let named = &attribute.properties[0];
            assert_eq!(named.name, "UnmanagedCode");
            assert_eq!(named.argument.ty.full_name(), "System.Boolean");
            assert_eq!(named.argument.value, CaValue::Boolean(true));
        }
        other => panic!("expected attribute declaration, got {other:?}"),
    }
}
