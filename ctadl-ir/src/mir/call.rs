use std::ops::Deref;
use std::{fmt, fmt::Display};

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use smallvec::SmallVec;
use thin_vec::ThinVec;

use super::{Symbol, VariableRef};

/// Which Java dispatch instruction a [`CallStyle::JavaCall`] came from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum JavaDispatch {
    /// Dex `invoke-virtual`, JVM `invokevirtual`: dispatch down a class hierarchy.
    Virtual,
    /// Dex `invoke-interface`, JVM `invokeinterface`: dispatch through an interface, which
    /// admits every unrelated class that implements it.
    Interface,
    /// Dex `invoke-super`, JVM `invokespecial` with a receiver: the target is fixed at the
    /// named class rather than found from the receiver.
    ///
    /// The two frontends do not draw this line in the same place, and a cross-frontend
    /// comparison has to know it. On dex, `invoke-direct` -- constructors and private methods
    /// -- lowers to a [`CallStyle::DirectCall`] and never reaches here, so `Super` is
    /// `invoke-super` alone. On the JVM the same three cases are one `invokespecial`, and the
    /// frontend lowers all of them to a `JavaCall`, so `Super` there also covers constructors
    /// and private calls. Both are true to what the instruction means; they count different
    /// instructions.
    Super,
    /// The frontend had no dispatch instruction to read: a JVM `invokedynamic` with a
    /// receiver, or a call built by hand (a test, a model). Not a fourth kind of dispatch --
    /// a gap in what was recorded, and reported as one rather than folded into `Virtual`.
    #[default]
    Unknown,
}

impl JavaDispatch {
    /// `"virtual"`, `"interface"`, `"super"`, `"unknown"`. The JSON spelling too.
    pub fn as_str(self) -> &'static str {
        match self {
            JavaDispatch::Virtual => "virtual",
            JavaDispatch::Interface => "interface",
            JavaDispatch::Super => "super",
            JavaDispatch::Unknown => "unknown",
        }
    }

    /// Every kind, in report order. The array rather than a derive, so a new variant has to
    /// be added here on purpose and every consumer iterating kinds picks it up at once.
    pub const ALL: [JavaDispatch; 4] = [
        JavaDispatch::Virtual,
        JavaDispatch::Interface,
        JavaDispatch::Super,
        JavaDispatch::Unknown,
    ];

    /// Dense index into a per-kind array, matching [`Self::ALL`].
    pub fn index(self) -> usize {
        match self {
            JavaDispatch::Virtual => 0,
            JavaDispatch::Interface => 1,
            JavaDispatch::Super => 2,
            JavaDispatch::Unknown => 3,
        }
    }
}

impl Display for JavaDispatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// [`Dispatch::Java`] enum.
    JavaCall {
        receiver: VariableRef,
        cls: Symbol,
        simple_name: Symbol,
        descriptor: Symbol,
        /// Which of `invoke-virtual` / `-interface` / `-super` this was; see [`JavaDispatch`].
        dispatch: JavaDispatch,
        /// For `dispatch: Super`, the class the runtime begins method lookup at. `None` for
        /// every other dispatch kind and for a `Super` site whose frontend could not
        /// determine it.
        ///
        /// Kept beside `cls` rather than overwriting it: `cls` is what a model matches and
        /// what `ctadl report` counts. The two differ because the instruction names the class
        /// of the *method reference*, which for a Dalvik `invoke-super` may be the current
        /// class rather than the superclass the runtime starts at.
        super_start: Option<Symbol>,
    },
    /// Lua `recv:m(...)` (or `recv.m(recv, ...)`); resolved via the metatable
    /// (`__index`) chain. Unlike [`CallStyle::JavaCall`] there is **no static
    /// `cls`** on the call: a Lua receiver has no declared type, so its class(es)
    /// come from allocation-site object facts on `receiver` at analysis time (see
    /// [`Dispatch::Lua`]). Codegen emits by method *name* and lets the
    /// object facts + CHA supply the class.
    LuaCall {
        receiver: VariableRef,
        method: Symbol,
    },
    /// .NET `callvirt`, resolved through [`Dispatch::Cil`]. Shaped like
    /// [`CallStyle::JavaCall`]: `cls` is the erased declaring type the instruction names (or the
    /// `constrained.` type), and `simple_name` + `descriptor` are the key CHA joins on. The
    /// receiver is not in the call's `args`. There is no dispatch kind: `callvirt` names an
    /// interface method and a class method alike, and the table says which `cls` is.
    CilCall {
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
            CallStyle::LuaCall { receiver, .. } => Some(receiver),
            CallStyle::CilCall { receiver, .. } => Some(receiver),
            CallStyle::FuncPtrCall { callee, .. } => Some(&callee.base),
            _ => None,
        }
    }

    pub fn receiver_mut(&mut self) -> Option<&mut VariableRef> {
        match self {
            CallStyle::JavaCall { receiver, .. } => Some(receiver),
            CallStyle::LuaCall { receiver, .. } => Some(receiver),
            CallStyle::CilCall { receiver, .. } => Some(receiver),
            CallStyle::FuncPtrCall { callee, .. } => Some(&mut callee.base),
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
                dispatch,
                super_start,
            } => {
                write!(
                    f,
                    "java-call {dispatch} {receiver}.<{cls}.{simple_name}{descriptor}>"
                )?;
                match super_start {
                    Some(start) => write!(f, " from {start}"),
                    None => Ok(()),
                }
            }
            LuaCall { receiver, method } => write!(f, "lua-call {receiver}:{method}"),
            CilCall {
                receiver,
                cls,
                simple_name,
                descriptor,
            } => write!(f, "cil-call {receiver}.<{cls}::{simple_name}{descriptor}>"),
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
    /// A Lua table tagged with the class table it was given a metatable from
    /// (`setmetatable({}, Account)` ⟹ `lua$class$Account`). Resolved against
    /// [`Dispatch::Lua`] at a [`CallStyle::LuaCall`] site.
    LuaClass(super::Symbol),
    /// A .NET object of the named class (`[Asm]Ns.Type`, the open generic definition), from
    /// `newobj`, `newarr` or `box`. Resolved against [`Dispatch::Cil`] at a
    /// [`CallStyle::CilCall`] site.
    CilObject(CilClass),
}

impl Display for CallObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallObject::FunctionPtr(name) => write!(f, "ptr<{name}>"),
            CallObject::JavaObject(cls) => write!(f, "java<{cls}>"),
            CallObject::LuaClass(cls) => write!(f, "lua<{cls}>"),
            CallObject::CilObject(cls) => write!(f, "cil<{cls}>"),
        }
    }
}

/// What an import says about its functions beyond their IR: the names each is known by, and
/// what call resolution needs.
///
/// The IR namespace is the only identity: every function has one fully-qualified IR name.
/// [`Self::names`] says, per IR name, what other names and facts apply to it, and every
/// frontend decides each function's simple name. Nothing here creates an identity.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VirtualMethodTable {
    /// One entry per IR function name a definition or call site uses, and at most one per name.
    pub names: Vec<FunctionNames>,
    pub dispatch: Dispatch,
}

/// What the table says about one IR function name.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FunctionNames {
    /// The IR name: the key, and the only identity.
    pub fq: Symbol,
    /// The name the source gives the function, without its class, module or namespace.
    pub simple: Symbol,
    /// Other names the function is known by, none of them `fq` or `simple`.
    pub other: SmallVec<[Symbol; 1]>,
    /// The class declaring it, where the language has classes.
    pub parent: Option<Symbol>,
    /// Its type signature, where the frontend has one.
    pub signature: Option<Symbol>,
}

impl FunctionNames {
    /// `fq`, known by `simple` and nothing else.
    pub fn new(fq: impl Into<Symbol>, simple: impl Into<Symbol>) -> Self {
        FunctionNames {
            fq: fq.into(),
            simple: simple.into(),
            other: SmallVec::new(),
            parent: None,
            signature: None,
        }
    }

    pub fn with_parent(mut self, parent: impl Into<Symbol>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    pub fn with_signature(mut self, signature: impl Into<Symbol>) -> Self {
        self.signature = Some(signature.into());
        self
    }

    /// Adds `name` to [`Self::other`], unless it is `fq`, `simple` or there already.
    pub fn with_other(mut self, name: impl Into<Symbol>) -> Self {
        let name = name.into();
        if name != self.fq && name != self.simple && !self.other.contains(&name) {
            self.other.push(name);
        }
        self
    }
}

impl Display for FunctionNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is named {}", self.fq, self.simple)?;
        for name in &self.other {
            write!(f, ", {name}")?;
        }
        if let Some(parent) = &self.parent {
            write!(f, " in {parent}")?;
        }
        if let Some(signature) = &self.signature {
            write!(f, " with signature {signature}")?;
        }
        Ok(())
    }
}

/// What call resolution needs, split out by language style.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Dispatch {
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
        /// The interfaces *this import declares*.
        ///
        /// `hierarchy` above merges a class's superclass with its super-interfaces into one
        /// parent list, which is what CHA wants -- both are subtype edges -- but it means the
        /// table alone cannot say which parents are interfaces. This column is that record.
        interfaces: Vec<JavaClass>,
        /// Method declarations carrying `abstract`, including every method of an interface
        /// that is not `default` or `static`.
        ///
        /// These are exactly the declarations that are **absent** from `methods`, which holds
        /// implementations: a body-less method has no code for the frontend to lower and no
        /// function to name. Recording them is what makes a single-abstract-method interface
        /// recognisable -- a functional interface is one interface with one row here -- which
        /// is otherwise underivable, since the CHA resolvent map keyed on an interface holds
        /// every method of every implementer rather than the interface's own.
        ///
        /// There is no fourth column: an abstract method has no implementation to name.
        abstract_methods: Vec<(JavaClass, JavaSimpleName, JavaSignature)>,
        /// Methods declared `native`. They also appear in `methods` above, so
        /// that CHA resolves a virtual call to one; this column is what the JNI
        /// bridge joins against, and it is the only one carrying the staticness
        /// the bridge's port map needs. A consumer walking both columns must
        /// therefore expect to see a native method twice.
        ///
        /// The columns are as follows:
        /// - Fully qualified class name declaring the method
        /// - Simple name of the method, e.g., nativeStash
        /// - Signature of the method, e.g., (Ljava/lang/String;)V
        /// - Fully qualified method name
        /// - Whether the method is `static` (it has no `this` parameter)
        natives: Vec<(JavaClass, JavaSimpleName, JavaSignature, JavaMethod, bool)>,
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
    /// Table for the Lua frontend. Structurally mirrors [`Dispatch::Java`]
    /// but has no descriptors/overloading: a Lua method is uniquely named within its
    /// class table, and the `__index` chain plays the role of `direct_superclass`.
    Lua {
        /// The columns are as follows:
        /// - Class table symbol defining the method (e.g. `lua$class$Account`)
        /// - Simple method name (e.g. `deposit`)
        /// - Fully-qualified function id the method lowers to (the IR function name)
        methods: Vec<(Symbol, Symbol, Symbol)>,
        /// Every function the frontend lowered, not just the class methods above. The columns
        /// are as follows:
        /// - Simple name, as the definition site spells it (`get_headers` in
        ///   `function kong.request.get_headers()`, `deposit` in `function Account:deposit()`,
        ///   `%chunk` / `%anonN` for a synthetic one)
        /// - Fully-qualified function name (the id used everywhere else), e.g.
        ///   `kong.pdk.request.get_headers`
        ///
        /// The frontend parses the simple name out of the definition's name node, so consumers
        /// read it here instead of re-deriving it from the qualified name: the two are not the
        /// same string operation, since a name collision within a module makes the IR name
        /// `<module>.f%1` while the function is still simply named `f`.
        functions: Vec<(Symbol, Symbol)>,
        /// Functions *called* by the import but defined nowhere in it — the Lua stdlib
        /// (`os.execute`, `string.format`), and anything from a module outside the import.
        /// Without this column they would be unmodelable: the IR names them correctly at their
        /// call sites, but every model match index is built from the table, so a model naming
        /// `execute` (or `qualified-id: "os.execute"`) would match nothing. dex/jvm answer the same question with their `Context::ext`
        /// entries and pcode with real `<EXTERNAL>::…` thunks.
        ///
        /// The columns are as follows:
        /// - Simple name
        /// - Fully-qualified name, exactly as the call site spells the callee
        ///
        /// Each external is registered under *both* spellings because Lua's two call syntaxes
        /// produce two different callee names for one library function: `string.format(x)`
        /// lowers to a call of `string.format`, while `s:format(x)` lowers to a call of the
        /// bare `format`. Unlike `functions` above, the simple name here is the last dotted
        /// component of the fq name rather than something read off a definition site — an
        /// external has no definition site, so splitting the name is the only source available.
        externals: Vec<(Symbol, Symbol)>,
        /// Subclass -> its `__index` parents (usually one). Mirrors Java `hierarchy`.
        hierarchy: HashMap<Symbol, SmallVec<[Symbol; 2]>>,
    },
    /// Table for the .NET (CIL) frontend. The first four columns mirror
    /// [`Dispatch::Java`], with .NET spellings: a class is `[Asm]Ns.Outer/Inner`, a
    /// descriptor `(P1,P2)->Ret`, a method id `{cls}::{simple}{descriptor}`.
    Cil {
        /// The columns are as follows:
        /// - Class whose table holds the row
        /// - Simple name, e.g. `Parse`, `.ctor`, `Select``2`
        /// - Descriptor, e.g. `(System.String)->System.Int32`
        /// - Method id the row resolves to
        ///
        /// Besides each class's own methods, this holds the *override bridges* the frontend
        /// synthesizes, as `javac` does for Java: `D : B<string>` overriding `B<T>.M(T)`
        /// gets a function `D::M(!0)` calling `D::M(string)`, and an explicit interface
        /// implementation one under the interface method's key. A bridge whose key is a
        /// different method of the class is named `{key}#{declaring class}`, so that one row
        /// does not name its own function.
        methods: Vec<(CilClass, CilSimpleName, CilSignature, CilMethod)>,
        /// Class -> its base type and the interfaces it implements, erased to open definitions.
        hierarchy: HashMap<CilClass, SmallVec<[CilClass; 2]>>,
        /// The interfaces this import declares. See the column of the same name on
        /// [`Dispatch::Java`].
        interfaces: Vec<CilClass>,
        /// Methods declared `abstract`, including interface methods without a default body.
        abstract_methods: Vec<(CilClass, CilSimpleName, CilSignature)>,
        /// Methods implemented by P/Invoke (`[DllImport]`). They also appear in `methods`.
        ///
        /// The columns are as follows:
        /// - Class, simple name, descriptor and method id, as in `methods`
        /// - The native module (`ModuleRef` name), e.g. `libc`
        /// - The entry point, e.g. `getenv`
        pinvokes: Vec<(
            CilClass,
            CilSimpleName,
            CilSignature,
            CilMethod,
            Symbol,
            Symbol,
        )>,
    },
}

/// The methods every class inherits from `java.lang.Object`, which an interface may redeclare
/// for documentation. Java's own functional-interface rule excludes them, so the
/// single-abstract-method test does too.
const OBJECT_METHODS: [(&str, &str); 3] = [
    ("toString", "()Ljava/lang/String;"),
    ("equals", "(Ljava/lang/Object;)Z"),
    ("hashCode", "()I"),
];

/// What a [`VirtualMethodTable`] says about types, as opposed to what a call site says about
/// dispatch. A call records the instruction it came from, whatever its receiver's type turns
/// out to be; this records what the import declares a type to be.
///
/// Only the types this import declares appear here. An interface from code that was not
/// imported is simply absent, so a lookup that fails means "not declared an interface here",
/// never "known not to be one".
#[derive(Debug, Default, Clone)]
pub struct TypeFacts {
    pub interfaces: HashSet<Symbol>,
    /// Interfaces with exactly one abstract method over their whole super-interface closure:
    /// the functional ones. See [`VirtualMethodTable::type_facts`].
    pub single_abstract_method: HashSet<Symbol>,
}

impl TypeFacts {
    /// Whether the import declares any interfaces at all.
    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty()
    }

    /// `None` when the import declares no interfaces at all. That prevents reading "this
    /// receiver is not an interface" off a program that had no way to say otherwise.
    pub fn receiver_is_interface(&self, cls: &Symbol) -> Option<bool> {
        (!self.is_empty()).then(|| self.interfaces.contains(cls))
    }
}

impl VirtualMethodTable {
    /// Which types this table declares to be interfaces, and which of those are
    /// single-abstract-method (functional) interfaces. Empty for a non-Java table.
    ///
    /// The test closes over a type's **transitive super-interfaces** rather than looking only
    /// at what the interface itself declares. `dagger.internal.Provider` declares no method of
    /// its own -- its one method is `get`, declared on the `javax.inject.Provider` it extends --
    /// and a declared-methods-only test misses it and every interface shaped like it.
    ///
    /// Two subtractions make the count mean what it says. Implementations declared anywhere in
    /// the closure are removed, because `methods` holds interface *default* methods, which are
    /// not abstract; without this a one-abstract-one-default interface reads as two. And
    /// `toString`, `equals` and `hashCode` are removed, matching Java's own rule for a
    /// functional interface.
    pub fn type_facts(&self) -> TypeFacts {
        let Dispatch::Java {
            methods,
            hierarchy,
            interfaces,
            abstract_methods,
            ..
        } = &self.dispatch
        else {
            return TypeFacts::default();
        };
        let interfaces: HashSet<Symbol> = interfaces.iter().map(|c| c.0.clone()).collect();
        // Distinct (name, descriptor) pairs per declaring type, rather than a running count.
        // One class can be declared in two dex files of the same app, and a method listed
        // twice is still one method.
        let mut declared: HashMap<Symbol, HashSet<(Symbol, Symbol)>> = HashMap::new();
        for (cls, name, desc) in abstract_methods {
            declared
                .entry(cls.0.clone())
                .or_default()
                .insert((name.0.clone(), desc.0.clone()));
        }
        let mut implemented: HashMap<Symbol, HashSet<(Symbol, Symbol)>> = HashMap::new();
        for (cls, name, desc, _id) in methods {
            implemented
                .entry(cls.0.clone())
                .or_default()
                .insert((name.0.clone(), desc.0.clone()));
        }
        let object_methods: HashSet<(Symbol, Symbol)> = OBJECT_METHODS
            .iter()
            .map(|(n, d)| (Symbol::from(*n), Symbol::from(*d)))
            .collect();

        let mut single_abstract_method = HashSet::new();
        let mut closure: Vec<Symbol> = Vec::new();
        let mut seen: HashSet<Symbol> = HashSet::new();
        for iface in &interfaces {
            closure.clear();
            seen.clear();
            closure.push(iface.clone());
            seen.insert(iface.clone());
            let mut next = 0;
            while next < closure.len() {
                let cls = closure[next].clone();
                next += 1;
                let Some(parents) = hierarchy.get(&JavaClass(cls)) else {
                    continue;
                };
                for parent in parents {
                    if interfaces.contains(&parent.0) && seen.insert(parent.0.clone()) {
                        closure.push(parent.0.clone());
                    }
                }
            }
            let mut abstracts: HashSet<(Symbol, Symbol)> = HashSet::new();
            for cls in &closure {
                if let Some(ms) = declared.get(cls) {
                    abstracts.extend(ms.iter().cloned());
                }
            }
            for cls in &closure {
                if let Some(ms) = implemented.get(cls) {
                    for m in ms {
                        abstracts.remove(m);
                    }
                }
            }
            for m in &object_methods {
                abstracts.remove(m);
            }
            if abstracts.len() == 1 {
                single_abstract_method.insert(iface.clone());
            }
        }
        TypeFacts {
            interfaces,
            single_abstract_method,
        }
    }

    /// A table with no names over `dispatch`.
    pub fn new(dispatch: Dispatch) -> Self {
        VirtualMethodTable {
            names: Vec::new(),
            dispatch,
        }
    }

    /// A table with no dispatch that names each function of `program` by its IR name, for a
    /// frontend whose IR names are the source's own (C, flowy).
    pub fn named_by_ir(program: &super::Program) -> Self {
        let mut vmt = VirtualMethodTable {
            names: program
                .functions
                .functions
                .iter()
                .map(|f| FunctionNames::new(f.name.as_str(), f.name.as_str()))
                .collect(),
            dispatch: Dispatch::Unknown,
        };
        vmt.finish_names();
        vmt
    }

    pub fn new_java() -> Self {
        Self::new(Dispatch::Java {
            methods: Vec::new(),
            hierarchy: HashMap::new(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        })
    }

    pub fn new_native() -> Self {
        Self::new(Dispatch::Native {
            methods: Vec::new(),
        })
    }

    pub fn new_cil() -> Self {
        Self::new(Dispatch::Cil {
            methods: Vec::new(),
            hierarchy: HashMap::new(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            pinvokes: Vec::new(),
        })
    }

    pub fn new_lua() -> Self {
        Self::new(Dispatch::Lua {
            methods: Vec::new(),
            functions: Vec::new(),
            externals: Vec::new(),
            hierarchy: HashMap::new(),
        })
    }

    /// Sorts `names` by IR name and keeps the first entry of each, for a frontend that meets a
    /// function twice (a callee, then its definition) or in no fixed order.
    pub fn finish_names(&mut self) {
        self.names.sort_by(|a, b| a.fq.cmp(&b.fq));
        self.names.dedup_by(|a, b| a.fq == b.fq);
    }

    /// Checks that `names` has at most one entry per IR name, and one for every name in
    /// `functions`. A frontend that leaves a function out, or describes one twice, has not
    /// decided its names.
    pub fn check_names<'a>(
        &self,
        functions: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), String> {
        let mut seen: HashSet<&str> = HashSet::with_capacity(self.names.len());
        for n in &self.names {
            if !seen.insert(n.fq.as_ref()) {
                return Err(format!("function {} has two name table entries", n.fq));
            }
        }
        let missing: Vec<&str> = functions
            .into_iter()
            .filter(|f| !seen.contains(f))
            .collect();
        match missing.as_slice() {
            [] => Ok(()),
            [first, ..] => Err(format!(
                "{} function(s) have no name table entry, the first {first}",
                missing.len()
            )),
        }
    }
}

impl Display for VirtualMethodTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.dispatch)?;
        if !self.names.is_empty() {
            if self.dispatch == Dispatch::Unknown {
                writeln!(f)?;
            }
            writeln!(f, "function names")?;
            for n in &self.names {
                writeln!(f, "{n}")?;
            }
            writeln!(f, "end function names")?;
        }
        Ok(())
    }
}

impl Display for Dispatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dispatch::Java {
                methods,
                hierarchy,
                interfaces,
                abstract_methods,
                natives,
            } => {
                writeln!(f, "java virtual method table")?;
                for (cls, name, sig, method) in methods {
                    writeln!(f, "{cls}.{name} has signature {sig}: {method}")?;
                }
                for cls in interfaces {
                    writeln!(f, "{cls} is an interface")?;
                }
                for (cls, name, sig) in abstract_methods {
                    writeln!(f, "{cls}.{name} has signature {sig}: abstract")?;
                }
                for (cls, name, sig, method, is_static) in natives {
                    let kind = if *is_static {
                        "static native"
                    } else {
                        "native"
                    };
                    writeln!(f, "{cls}.{name} has signature {sig}: {method} ({kind})")?;
                }
                for (subclass, superclasses) in hierarchy {
                    for superclass in superclasses {
                        writeln!(f, "{subclass} extends {superclass}")?;
                    }
                }
                writeln!(f, "end java virtual method table")?;
                Ok(())
            }
            Dispatch::Native { methods } => {
                writeln!(f, "native virtual method table")?;
                for (name, sig, func, qualified) in methods {
                    writeln!(f, "{name}{sig}: {func} (qualified {qualified})")?;
                }
                writeln!(f, "end native virtual method table")?;
                Ok(())
            }
            Dispatch::Lua {
                methods,
                functions,
                externals,
                hierarchy,
            } => {
                writeln!(f, "lua virtual method table")?;
                for (cls, name, func) in methods {
                    writeln!(f, "{cls}.{name}: {func}")?;
                }
                for (name, func) in functions {
                    writeln!(f, "{name}: {func}")?;
                }
                for (name, func) in externals {
                    writeln!(f, "{name}: {func} (external)")?;
                }
                for (subclass, superclasses) in hierarchy {
                    for superclass in superclasses {
                        writeln!(f, "{subclass} extends {superclass}")?;
                    }
                }
                writeln!(f, "end lua virtual method table")?;
                Ok(())
            }
            Dispatch::Cil {
                methods,
                hierarchy,
                interfaces,
                abstract_methods,
                pinvokes,
            } => {
                writeln!(f, "cil virtual method table")?;
                for (cls, name, sig, method) in methods {
                    writeln!(f, "{cls}::{name} has signature {sig}: {method}")?;
                }
                for cls in interfaces {
                    writeln!(f, "{cls} is an interface")?;
                }
                for (cls, name, sig) in abstract_methods {
                    writeln!(f, "{cls}::{name} has signature {sig}: abstract")?;
                }
                for (cls, name, sig, method, module, entry) in pinvokes {
                    writeln!(
                        f,
                        "{cls}::{name} has signature {sig}: {method} (pinvoke {module}!{entry})"
                    )?;
                }
                let mut edges: Vec<_> = hierarchy.iter().collect();
                edges.sort_by(|a, b| a.0.0.cmp(&b.0.0));
                for (subclass, superclasses) in edges {
                    for superclass in superclasses {
                        writeln!(f, "{subclass} extends {superclass}")?;
                    }
                }
                writeln!(f, "end cil virtual method table")?;
                Ok(())
            }
            Dispatch::Unknown => write!(f, "unknown virtual method table"),
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

/// A newtype over [`Symbol`] with the derives and conversions every VMT column type has.
macro_rules! symbol_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #[repr(transparent)]
        pub struct $name(pub Symbol);

        impl Deref for $name {
            type Target = Symbol;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl From<$name> for Symbol {
            fn from(c: $name) -> Self {
                c.0
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

symbol_newtype!(
    /// A .NET class: `[Asm]Ns.Outer/Inner`, generic arity kept, open definition.
    CilClass
);
symbol_newtype!(
    /// A .NET method's simple name: `Parse`, `.ctor`, `Select``2`.
    CilSimpleName
);
symbol_newtype!(
    /// A .NET method descriptor: `(System.String,!!0&)->System.Boolean`.
    CilSignature
);
symbol_newtype!(
    /// A .NET method id: `{cls}::{simple}{descriptor}`.
    CilMethod
);

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

#[cfg(test)]
mod type_facts_tests {
    use super::*;

    fn java(
        interfaces: &[&str],
        hierarchy: &[(&str, &[&str])],
        abstract_methods: &[(&str, &str, &str)],
        methods: &[(&str, &str, &str)],
    ) -> VirtualMethodTable {
        VirtualMethodTable::new(Dispatch::Java {
            methods: methods
                .iter()
                .map(|(c, n, d)| {
                    (
                        JavaClass((*c).into()),
                        JavaSimpleName((*n).into()),
                        JavaSignature((*d).into()),
                        JavaMethod(format!("{c}->{n}{d}").into()),
                    )
                })
                .collect(),
            hierarchy: hierarchy
                .iter()
                .map(|(sub, sups)| {
                    (
                        JavaClass((*sub).into()),
                        sups.iter().map(|s| JavaClass((*s).into())).collect(),
                    )
                })
                .collect(),
            interfaces: interfaces.iter().map(|c| JavaClass((*c).into())).collect(),
            abstract_methods: abstract_methods
                .iter()
                .map(|(c, n, d)| {
                    (
                        JavaClass((*c).into()),
                        JavaSimpleName((*n).into()),
                        JavaSignature((*d).into()),
                    )
                })
                .collect(),
            natives: Vec::new(),
        })
    }

    fn is_sam(vmt: &VirtualMethodTable, cls: &str) -> bool {
        vmt.type_facts()
            .single_abstract_method
            .contains(&Symbol::from(cls))
    }

    /// `dagger.internal.Provider` declares nothing of its own: its one method comes from the
    /// `javax.inject.Provider` it extends. A declared-methods-only test misses both.
    #[test]
    fn inherited_abstract_method_counts() {
        let vmt = java(
            &["Ljavax/inject/Provider;", "Ldagger/internal/Provider;"],
            &[(
                "Ldagger/internal/Provider;",
                &["Ljavax/inject/Provider;"][..],
            )],
            &[("Ljavax/inject/Provider;", "get", "()Ljava/lang/Object;")],
            &[],
        );
        assert!(is_sam(&vmt, "Ldagger/internal/Provider;"));
        assert!(is_sam(&vmt, "Ljavax/inject/Provider;"));
    }

    /// A default method has a body, so it is in `methods` rather than `abstract_methods`.
    /// Without subtracting implementations the interface reads as having two.
    #[test]
    fn default_method_is_not_abstract() {
        let vmt = java(
            &["LI;"],
            &[],
            &[("LI;", "run", "()V"), ("LI;", "helper", "()V")],
            &[("LI;", "helper", "()V")],
        );
        assert!(is_sam(&vmt, "LI;"));
    }

    /// Interfaces redeclare `equals` for documentation. Java's functional-interface rule
    /// ignores it, so this one is still single-abstract-method.
    #[test]
    fn redeclared_object_method_is_ignored() {
        let vmt = java(
            &["LI;"],
            &[],
            &[
                ("LI;", "run", "()V"),
                ("LI;", "equals", "(Ljava/lang/Object;)Z"),
            ],
            &[],
        );
        assert!(is_sam(&vmt, "LI;"));
    }

    /// Two genuinely abstract methods in the closure is not a functional interface.
    #[test]
    fn two_abstract_methods_is_not_functional() {
        let vmt = java(
            &["LBase;", "LI;"],
            &[("LI;", &["LBase;"][..])],
            &[("LBase;", "a", "()V"), ("LI;", "b", "()V")],
            &[],
        );
        assert!(!is_sam(&vmt, "LI;"));
        assert!(is_sam(&vmt, "LBase;"));
    }

    /// A class extending a functional interface is not itself one: only interfaces are.
    #[test]
    fn only_interfaces_qualify() {
        let vmt = java(
            &["LI;"],
            &[("LC;", &["LI;"][..])],
            &[("LI;", "run", "()V")],
            &[("LC;", "run", "()V")],
        );
        assert!(!is_sam(&vmt, "LC;"));
    }
}
