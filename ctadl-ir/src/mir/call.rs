use std::ops::Deref;
use std::{fmt, fmt::Display};

use hashbrown::hash_map::HashMap;
use smallvec::SmallVec;
use thin_vec::ThinVec;

use super::{Symbol, VariableRef};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CallStyle {
    Unknown,
    DirectCall {
        call_edges: CallEdges,
    },
    /// C function pointer call
    FuncPtrCall {
        callee: super::AccessPath,
        signature: Option<String>,
    },
    /// Java virtual call. At analysis time, this consults metadata in the
    /// [`VirtualMethodTable::Java`] enum.
    JavaCall {
        receiver: VariableRef,
        cls: Symbol,
        simple_name: Symbol,
        descriptor: Symbol,
    },
}

impl CallStyle {
    pub fn receiver(&self) -> Option<&VariableRef> {
        match self {
            CallStyle::JavaCall { receiver, .. } => Some(receiver),
            CallStyle::FuncPtrCall { callee, .. } => Some(&callee.variable_ref),
            _ => None,
        }
    }

    pub fn receiver_mut(&mut self) -> Option<&mut VariableRef> {
        match self {
            CallStyle::JavaCall { receiver, .. } => Some(receiver),
            CallStyle::FuncPtrCall { callee, .. } => Some(&mut callee.variable_ref),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CallEdges {
    /// List of call edges for this call. Can be empty.
    Explicit(ThinVec<String>),
}

impl CallEdges {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            CallEdges::Explicit(e) => e.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Display for CallStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use CallStyle::*;
        match self {
            Unknown => write!(f, "<unknown-call>"),
            DirectCall { call_edges } => {
                write!(f, "direct-call {call_edges}")
            }
            JavaCall {
                receiver,
                cls,
                simple_name,
                descriptor,
            } => write!(f, "java-call {receiver}.<{cls}.{simple_name}{descriptor}>"),
            FuncPtrCall { callee, signature } => match signature {
                Some(signature) => write!(f, "funcptr-call {callee} <{signature}>"),
                None => write!(f, "funcptr-call {callee}"),
            },
        }
    }
}

impl Display for CallEdges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let CallEdges::Explicit(edges) = self;
        if edges.len() > 1 {
            write!(f, "{} and {} others", edges[0], edges.len() - 1)
        } else if edges.len() == 1 {
            write!(f, "{}", edges[0])
        } else {
            write!(f, "{} edges", edges.len())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CallObject {
    FunctionPtr(super::Symbol),
    JavaObject(JavaClass),
}

impl Display for CallObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallObject::FunctionPtr(name) => write!(f, "ptr<{name}>"),
            CallObject::JavaObject(cls) => write!(f, "java<{cls}>"),
        }
    }
}

/// Virtual method table representation, split out by language style.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum VirtualMethodTable {
    #[default]
    Unknown,
    Java {
        /// The columns are as follows:
        /// - Fully qualified class name defining the method
        /// - Simple name of the method, e.g., getString
        /// - Signature of the method, e.g., (IILcom/example/Foo;)V
        /// - Fully qualified method name
        methods: Vec<(JavaClass, JavaSimpleName, JavaSignature, JavaMethod)>,
        hierarchy: HashMap<JavaClass, SmallVec<[JavaClass; 2]>>,
    },
    /// Table for native / binary frontends (pcode today, clang later). There is
    /// no class hierarchy; each function contributes its simple (un-decorated)
    /// name and a best-effort type signature so JSON models can match by name or
    /// `signature_pattern` even when the IR's fully-qualified name is decorated
    /// (e.g. Ghidra names an imported `system` as `<EXTERNAL>::system@00101008`).
    Native {
        /// The columns are as follows:
        /// - Simple name of the function, e.g. `system`
        /// - Type signature of the function, e.g. `(int, char**)`
        /// - Fully-qualified function name (the id used everywhere else)
        /// - Namespace-qualified name, e.g. `Foo::bar` (see [`NativeQualifiedName`])
        methods: Vec<(
            NativeSimpleName,
            NativeSignature,
            NativeFunction,
            NativeQualifiedName,
        )>,
    },
    CplusPlus,
}

impl VirtualMethodTable {
    pub fn new_java() -> Self {
        VirtualMethodTable::Java {
            methods: Vec::new(),
            hierarchy: HashMap::new(),
        }
    }

    pub fn new_native() -> Self {
        VirtualMethodTable::Native {
            methods: Vec::new(),
        }
    }
}

impl Display for VirtualMethodTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VirtualMethodTable::Java { methods, hierarchy } => {
                writeln!(f, "java virtual method table")?;
                for (cls, name, sig, method) in methods {
                    writeln!(f, "{cls}.{name} has signature {sig}: {method}")?;
                }
                for (subclass, superclasses) in hierarchy {
                    for superclass in superclasses {
                        writeln!(f, "{subclass} extends {superclass}")?;
                    }
                }
                writeln!(f, "end java virtual method table")?;
                Ok(())
            }
            VirtualMethodTable::Native { methods } => {
                writeln!(f, "native virtual method table")?;
                for (name, sig, func, qualified) in methods {
                    writeln!(f, "{name}{sig}: {func} (qualified {qualified})")?;
                }
                writeln!(f, "end native virtual method table")?;
                Ok(())
            }
            VirtualMethodTable::Unknown => write!(f, "unknown virtual method table"),
            VirtualMethodTable::CplusPlus => write!(f, "empty c++ virtual method table"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(transparent)]
pub struct JavaClass(pub Symbol);

impl Deref for JavaClass {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<JavaClass> for Symbol {
    fn from(c: JavaClass) -> Self {
        c.0.clone()
    }
}

impl Display for JavaClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct JavaSimpleName(pub Symbol);

impl Deref for JavaSimpleName {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<JavaSimpleName> for Symbol {
    fn from(c: JavaSimpleName) -> Self {
        c.0.clone()
    }
}

impl Display for JavaSimpleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct JavaSignature(pub Symbol);

impl Deref for JavaSignature {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<JavaSignature> for Symbol {
    fn from(c: JavaSignature) -> Self {
        c.0.clone()
    }
}

impl Display for JavaSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct JavaMethod(pub Symbol);

impl Deref for JavaMethod {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<JavaMethod> for Symbol {
    fn from(c: JavaMethod) -> Self {
        c.0.clone()
    }
}

impl Display for JavaMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Simple (un-decorated) name of a native function, e.g. `system`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NativeSimpleName(pub Symbol);

impl Deref for NativeSimpleName {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<NativeSimpleName> for Symbol {
    fn from(c: NativeSimpleName) -> Self {
        c.0.clone()
    }
}

impl Display for NativeSimpleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Type signature of a native function, e.g. `(int, char**)`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NativeSignature(pub Symbol);

impl Deref for NativeSignature {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<NativeSignature> for Symbol {
    fn from(c: NativeSignature) -> Self {
        c.0.clone()
    }
}

impl Display for NativeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Fully-qualified name of a native function (the id used everywhere else),
/// e.g. `<EXTERNAL>::system@00101008` or `main`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NativeFunction(pub Symbol);

impl Deref for NativeFunction {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<NativeFunction> for Symbol {
    fn from(c: NativeFunction) -> Self {
        c.0.clone()
    }
}

impl Display for NativeFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Namespace-qualified name of a native function, e.g. `Foo::bar` or
/// `<EXTERNAL>::system`.
///
/// Unlike [`NativeFunction`] this carries no address, so it is stable across
/// binaries; unlike [`NativeSimpleName`] it keeps the enclosing namespace, so two
/// same-named methods in different namespaces stay distinguishable. Frontends that
/// cannot recover a namespace populate it with the simple name.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NativeQualifiedName(pub Symbol);

impl Deref for NativeQualifiedName {
    type Target = Symbol;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<NativeQualifiedName> for Symbol {
    fn from(c: NativeQualifiedName) -> Self {
        c.0.clone()
    }
}

impl Display for NativeQualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
