//! Layer 1: the Cecil-shaped, lazy, borrow-based object model.

pub mod body;
pub mod custom_attributes;
pub(crate) mod data;
pub mod marshal;
pub mod handles;
pub mod module;
pub mod names;
pub mod resolver;
pub mod sig;
pub mod typeparser;
pub(crate) mod util;

pub use data::Range;
pub use body::{ExceptionHandler, ExceptionHandlerType, MethodBody};
pub use custom_attributes::{CaArgument, CaNamedArgument, CaType, CaValue, CaValues, CustomAttribute, SecurityAttribute, SecurityDeclaration, SecurityDeclarationValue};
pub use handles::*;
pub use marshal::MarshalInfo;
pub use module::{AssemblyName, Entity, Module, ModuleRef, Resource};
pub use names::GenericContext;
pub use resolver::{AssemblyCache, AssemblyLoader, DirectoryLoader, ResolvedMember, ResolvedType};
pub use sig::{ArrayDimension, ElementType, MethodSig, PropertySig, SigReader, TypeSig};
pub use typeparser::ParsedType;
