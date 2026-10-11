// GapGen <outdir>: writes gap-<name>.dll, small modules with metadata shapes
// that neither Roslyn nor ilasm emit but obfuscators and other writers do,
// for comparing cil-reader against Cecil. Layouts that Cecil misreads and
// the CLR reads go in <outdir>/clr, which that comparison does not scan.
// The .il fixtures in readers/cil-reader/fixtures cover what ilasm can
// express.
//
// Each module is built with dnlib's object model, then the rows that model
// cannot express are patched in the metadata tables once dnlib has created
// them (MDEndCreateTables, before the heaps are sealed). The output is
// deterministic: fixed MVIDs and PE timestamps.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using dnlib.DotNet;
using dnlib.DotNet.Emit;
using dnlib.DotNet.MD;
using dnlib.DotNet.Writer;
using Metadata = dnlib.DotNet.Writer.Metadata;

static class Program {
    const MethodImplAttributes Impl = MethodImplAttributes.IL | MethodImplAttributes.Managed;
    const MethodAttributes Instance = MethodAttributes.Public | MethodAttributes.HideBySig;
    const MethodAttributes Static = Instance | MethodAttributes.Static;
    const MethodAttributes Accessor = Instance | MethodAttributes.SpecialName;
    const MethodAttributes Ctor = Instance | MethodAttributes.SpecialName | MethodAttributes.RTSpecialName;

    static int Main(string[] args) {
        if (args.Length != 1) {
            Console.Error.WriteLine("usage: GapGen <outdir>");
            return 2;
        }
        Directory.CreateDirectory(args[0]);
        var fixtures = new (string, Func<(ModuleDef, Action<Metadata>)>)[] {
            ("nested-public", NestedPublic),
            ("generic-names", GenericNames),
            ("methoddef-parent", MethodDefParent),
            ("property-sigs", PropertySigs),
            ("enum-scopes", EnumScopes),
            ("key-no-flag", () => PublicKeys("key-no-flag", flag: false, key: Key)),
            ("flag-no-key", () => PublicKeys("flag-no-key", flag: true, key: Array.Empty<byte>())),
            ("resolve-lib", ResolveLib),
            ("resolve", Resolve),
            ("base-specs", BaseSpecs),
            ("deleted", Deleted),
            ("enum-moved", EnumMoved),
            ("enum-lib", EnumLib),
            ("enums", Enums),
            ("utf8-names", Utf8Names),
            ("heap-indexes", HeapIndexes),
            ("typeref-scopes", TypeRefScopes),
        };
        foreach (var (name, build) in fixtures) {
            var (mod, patch) = build();
            var opts = new ModuleWriterOptions(mod);
            opts.PEHeadersOptions.TimeDateStamp = 0;
            opts.WriterEvent += (_, e) => {
                if (e.Event == ModuleWriterEvent.MDEndCreateTables)
                    patch(e.Writer.Metadata);
            };
            var path = Path.Combine(args[0], $"gap-{name}.dll");
            mod.Write(path, opts);
            Console.WriteLine($"wrote {path}");
        }
        // Modules whose bytes are patched once written: the metadata root,
        // or method bodies.
        var images = new (string, Func<ModuleDef>, Action<byte[]>)[] {
            ("streams", () => Layout("streams"), Streams),
            ("eh-sections", EhSections, EhSectionHeaders),
            ("operand-tokens", OperandTokens, OperandTokenRows),
            ("local-tokens", LocalTokens, LocalTokenValues),
            ("code-sizes", CodeSizes, CodeSizeValues),
        };
        foreach (var (name, build, patch) in images) {
            var mod = build();
            var opts = new ModuleWriterOptions(mod);
            opts.PEHeadersOptions.TimeDateStamp = 0;
            AddEmptyStream(opts, "#XS");
            AddEmptyStream(opts, "#YS");
            var bytes = new MemoryStream();
            mod.Write(bytes, opts);
            var image = bytes.ToArray();
            patch(image);
            var path = Path.Combine(args[0], $"gap-{name}.dll");
            File.WriteAllBytes(path, image);
            Console.WriteLine($"wrote {path}");
        }
        // One module in several tables-stream layouts.
        var layouts = new (string, Action<ModuleWriterOptions>)[] {
            ("enc", o => o.MetadataOptions.TablesHeapOptions.UseENC = true),
            // #JTD with #~ tables: the CLR's read-only metadata ignores it,
            // as Cecil does.
            ("jtd-compressed", o => AddEmptyStream(o, "#JTD")),
            // HeapSizes' ExtraData bit: a u32 after the row counts, which
            // Cecil reads as the start of the Module table.
            ("clr/extra-data", o => o.MetadataOptions.TablesHeapOptions.ExtraData = 0x0BADF00D),
            // #JTD with #- tables: the CLR makes every index 4 bytes, where
            // Cecil sizes them from HeapSizes and the row counts.
            ("clr/minimal-delta", o => MinimalDelta(o, "#JTD")),
            ("clr/minimal-delta-case", o => MinimalDelta(o, "#jtd")),
        };
        foreach (var (name, layout) in layouts) {
            var dir = Path.Combine(args[0], Path.GetDirectoryName(name));
            var stem = Path.GetFileName(name);
            Directory.CreateDirectory(dir);
            var mod = Layout(stem);
            var opts = new ModuleWriterOptions(mod);
            opts.PEHeadersOptions.TimeDateStamp = 0;
            layout(opts);
            var path = Path.Combine(dir, $"gap-{stem}.dll");
            mod.Write(path, opts);
            Console.WriteLine($"wrote {path}");
        }
        return 0;
    }

    static void MinimalDelta(ModuleWriterOptions opts, string name) {
        opts.MetadataOptions.TablesHeapOptions.UseENC = true;
        // Leaves HeapSizes' size bits clear: only #JTD makes the indexes 4
        // bytes.
        opts.MetadataOptions.TablesHeapOptions.ForceBigColumns = true;
        AddEmptyStream(opts, name);
    }

    // Right after the tables stream. dnlib puts custom heaps last, and a
    // stream that starts at the end of the metadata fails the CLR's loader.
    static void AddEmptyStream(ModuleWriterOptions opts, string name) =>
        opts.MetadataOptions.MetadataHeapsAdded += (_, e) => e.Heaps.Insert(1, new EmptyHeap(name));

    // A stream with no data, which only marks the metadata.
    sealed class EmptyHeap : HeapBase {
        readonly string name;
        public EmptyHeap(string name) => this.name = name;
        public override string Name => name;
        public override uint GetRawLength() => 0;
        protected override void WriteToImpl(DataWriter writer) { }
    }

    // A module that uses each heap and coded index kind the layouts change:
    // strings, user strings, blobs, a GUID, TypeRefs, a MemberRef, a nested
    // type and a custom attribute. L.C.Run() returns "run:cctor".
    static ModuleDef Layout(string name) {
        var mod = NewModule(name);
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "L", "C");
        mod.Types.Add(c);
        var s = new FieldDefUser("S", new FieldSig(cs.String), FieldAttributes.Public | FieldAttributes.Static);
        c.Fields.Add(s);
        var cctor = Method(".cctor", MethodSig.CreateStatic(cs.Void),
            MethodAttributes.Private | MethodAttributes.HideBySig | MethodAttributes.Static
                | MethodAttributes.SpecialName | MethodAttributes.RTSpecialName,
            OpCodes.Ldstr.ToInstruction("cctor"), OpCodes.Stsfld.ToInstruction(s));
        c.Methods.Add(cctor);
        var concat = new MemberRefUser(mod, "Concat", MethodSig.CreateStatic(cs.String, cs.String, cs.String),
            cs.String.TypeDefOrRef);
        c.Methods.Add(Method("Run", MethodSig.CreateStatic(cs.String), Static,
            OpCodes.Ldstr.ToInstruction("run:"), OpCodes.Ldsfld.ToInstruction(s), OpCodes.Call.ToInstruction(concat)));
        var nested = new TypeDefUser("", "N", cs.Object.TypeDefOrRef) {
            Attributes = TypeAttributes.NestedPublic,
        };
        c.NestedTypes.Add(nested);
        nested.Fields.Add(new FieldDefUser("x", new FieldSig(cs.Int64), FieldAttributes.Public));
        var obsolete = new TypeRefUser(mod, "System", "ObsoleteAttribute", cs.AssemblyRef);
        var obsoleteCtor = new MemberRefUser(mod, ".ctor", MethodSig.CreateInstance(cs.Void, cs.String), obsolete);
        c.CustomAttributes.Add(new CustomAttribute(obsoleteCtor, new[] { new CAArgument(cs.String, "old") }));
        return mod;
    }

    // A library module named gap-<name>, referencing mscorlib 4.0, with an
    // MVID derived from its name.
    static ModuleDefUser NewModule(string name) {
        var mvid = new Guid(MD5.HashData(Encoding.UTF8.GetBytes(name)));
        var mod = new ModuleDefUser($"gap-{name}.dll", mvid, AssemblyRefUser.CreateMscorlibReferenceCLR40()) {
            Kind = ModuleKind.Dll,
            RuntimeVersion = MDHeaderRuntimeVersion.MS_CLR_40,
        };
        new AssemblyDefUser($"gap-{name}", new Version(1, 0, 0, 0)).Modules.Add(mod);
        return mod;
    }

    static TypeDefUser NewClass(ModuleDef mod, string ns, string name) {
        var t = new TypeDefUser(ns, name, mod.CorLibTypes.Object.TypeDefOrRef) {
            Attributes = TypeAttributes.Public | TypeAttributes.BeforeFieldInit,
        };
        return t;
    }

    static MethodDef Method(string name, MethodSig sig, MethodAttributes attrs, params Instruction[] body) {
        var m = new MethodDefUser(name, sig, Impl, attrs) { Body = new CilBody() };
        foreach (var i in body)
            m.Body.Instructions.Add(i);
        m.Body.Instructions.Add(OpCodes.Ret.ToInstruction());
        return m;
    }

    static void Patch<T>(MDTable<T> table, uint rid, Func<T, T> f) where T : struct => table[rid] = f(table[rid]);

    // A NestedClass row for a type whose visibility is not
    // Nested*. Cecil only sets DeclaringType for nested visibilities, so
    // Inner and its members are named as top-level.
    static (ModuleDef, Action<Metadata>) NestedPublic() {
        var mod = NewModule("nested-public");
        var cs = mod.CorLibTypes;
        var outer = NewClass(mod, "N", "Outer");
        var inner = NewClass(mod, "", "Inner");
        inner.Visibility = TypeAttributes.NestedPublic;
        var fuel = NewClass(mod, "", "Fuel");
        fuel.Visibility = TypeAttributes.NestedPublic;
        var private_ = NewClass(mod, "", "Private");
        private_.Visibility = TypeAttributes.NestedPrivate;
        mod.Types.Add(outer);
        outer.NestedTypes.Add(inner);
        outer.NestedTypes.Add(private_);
        inner.NestedTypes.Add(fuel);

        var f = new FieldDefUser("f", new FieldSig(new ClassSig(fuel)), FieldAttributes.Public);
        inner.Fields.Add(f);
        var m = Method("M", MethodSig.CreateStatic(cs.Void, new ClassSig(inner)), Static);
        inner.Methods.Add(m);
        var user = Method("Use", MethodSig.CreateStatic(cs.Void), Static,
            OpCodes.Ldnull.ToInstruction(),
            OpCodes.Call.ToInstruction(m),
            OpCodes.Ldnull.ToInstruction(),
            OpCodes.Ldfld.ToInstruction(f),
            OpCodes.Pop.ToInstruction());
        outer.Methods.Add(user);

        return (mod, md => Patch(md.TablesHeap.TypeDefTable, md.GetRid(inner), r =>
            new RawTypeDefRow((r.Flags & ~(uint)TypeAttributes.VisibilityMask) | (uint)TypeAttributes.Public,
                r.Name, r.Namespace, r.Extends, r.FieldList, r.MethodList)));
    }

    // Generic parameters with empty names, as obfuscators leave
    // them. Cecil names them !n / !!n by position.
    static (ModuleDef, Action<Metadata>) GenericNames() {
        var mod = NewModule("generic-names");
        var cs = mod.CorLibTypes;
        var g = NewClass(mod, "", "G`2");
        var t0 = new GenericParamUser(0, GenericParamAttributes.NonVariant, "T0");
        var t1 = new GenericParamUser(1, GenericParamAttributes.NonVariant, "B");
        g.GenericParameters.Add(t0);
        g.GenericParameters.Add(t1);
        mod.Types.Add(g);

        var nullable = new TypeRefUser(mod, "System", "Nullable`1", mod.CorLibTypes.AssemblyRef);
        var var0 = new GenericVar(0, g);
        var var1 = new GenericVar(1, g);
        g.Fields.Add(new FieldDefUser("f", new FieldSig(var0), FieldAttributes.Public));
        g.Fields.Add(new FieldDefUser("p", new FieldSig(new PtrSig(new GenericInstSig(new ValueTypeSig(nullable), var0))), FieldAttributes.Public));
        g.Fields.Add(new FieldDefUser("b", new FieldSig(var1), FieldAttributes.Public));

        var sig = MethodSig.CreateInstanceGeneric(1, new GenericMVar(0), new GenericMVar(0), var0);
        var m = new MethodDefUser("M", sig, Impl, Instance) { Body = new CilBody() };
        var u0 = new GenericParamUser(0, GenericParamAttributes.NonVariant, "U0");
        m.GenericParameters.Add(u0);
        g.Methods.Add(m);
        // A call through G`2<!0,!1>::M<!0>, so operands render the names too.
        var self = new GenericInstSig(new ClassSig(g), var0, var1);
        var mref = new MemberRefUser(mod, "M", sig, new TypeSpecUser(self));
        var mspec = new MethodSpecUser(mref, new GenericInstMethodSig(var0));
        m.Body.Variables.Add(new Local(new GenericMVar(0)));
        m.Body.Variables.Add(new Local(var0));
        foreach (var i in new[] {
            OpCodes.Ldarg_0.ToInstruction(),
            OpCodes.Ldarg_2.ToInstruction(),
            OpCodes.Ldarg_2.ToInstruction(),
            OpCodes.Call.ToInstruction(mspec),
            OpCodes.Pop.ToInstruction(),
            OpCodes.Ldarg_1.ToInstruction(),
            OpCodes.Ret.ToInstruction(),
        })
            m.Body.Instructions.Add(i);

        return (mod, md => {
            foreach (var gp in new[] { t0, u0 })
                Patch(md.TablesHeap.GenericParamTable, md.GetRid(gp), r =>
                    new RawGenericParamRow(r.Number, r.Flags, r.Owner, 0, r.Kind));
        });
    }

    // A MemberRef whose parent is a generic MethodDef, the form of a
    // vararg call to a MethodDef. Cecil reads its signature against a new
    // MethodReference, so !0 takes the declaring type's name and !!0 stays
    // unbound. (A vararg generic method is left out: ECMA and C# forbid it,
    // and Cecil does not resolve calls to one.)
    static (ModuleDef, Action<Metadata>) MethodDefParent() {
        var mod = NewModule("methoddef-parent");
        var cs = mod.CorLibTypes;
        var g = NewClass(mod, "", "G`1");
        g.GenericParameters.Add(new GenericParamUser(0, GenericParamAttributes.NonVariant, "X"));
        mod.Types.Add(g);
        var var0 = new GenericVar(0, g);
        var mvar0 = new GenericMVar(0);

        var v = Method("V", MethodSig.CreateStaticGeneric(1, cs.Void, mvar0, var0), Static);
        v.GenericParameters.Add(new GenericParamUser(0, GenericParamAttributes.NonVariant, "T"));
        g.Methods.Add(v);

        var mref = new MemberRefUser(mod, "V", MethodSig.CreateStatic(cs.Void, mvar0, var0), v);
        g.Methods.Add(Method("Caller", MethodSig.CreateStatic(cs.Void), Static,
            OpCodes.Ldnull.ToInstruction(),
            OpCodes.Ldnull.ToInstruction(),
            OpCodes.Call.ToInstruction(mref)));
        return (mod, _ => { });
    }

    // Property signatures whose parameters are junk. Cecil
    // reads the parameter count, ignores it, and reads only the type.
    static (ModuleDef, Action<Metadata>) PropertySigs() {
        var mod = NewModule("property-sigs");
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        var junk = new List<(PropertyDef, byte[])>();
        void Add(string name, bool getter, params byte[] blob) {
            var p = new PropertyDefUser(name, PropertySig.CreateInstance(cs.Int32));
            if (getter) {
                var get = Method("get_" + name, MethodSig.CreateInstance(cs.Int32), Accessor, OpCodes.Ldc_I4_0.ToInstruction());
                c.Methods.Add(get);
                p.GetMethod = get;
            }
            c.Properties.Add(p);
            junk.Add((p, blob));
        }
        // HASTHIS|PROPERTY, a count of 2, I4, then nothing.
        Add("Truncated", true, 0x28, 0x02, 0x08);
        // One parameter of an undefined element type.
        Add("BadElement", true, 0x28, 0x01, 0x08, 0x17);
        // The same, with no accessors.
        Add("NoAccessors", false, 0x28, 0x01, 0x08, 0x17);
        // Parameters, then trailing bytes.
        Add("Trailing", true, 0x28, 0x01, 0x08, 0x0e, 0xff, 0xff);

        return (mod, md => {
            foreach (var (p, blob) in junk) {
                var index = md.BlobHeap.Add(blob);
                Patch(md.TablesHeap.PropertyTable, md.GetRid(p), r => new RawPropertyRow(r.PropFlags, r.Name, index));
            }
        });
    }

    // Custom-attribute enum arguments whose TypeRef is scoped to
    // this module, to nothing, or to a module reference. Cecil resolves a
    // module-scoped or unscoped name through the module's exported types
    // too, and a module reference only in the assembly's modules of that
    // name.
    static (ModuleDef, Action<Metadata>) EnumScopes() {
        var mod = NewModule("enum-scopes");
        var cs = mod.CorLibTypes;
        var en = new TypeDefUser("Local", "En", mod.CorLibTypes.GetTypeRef("System", "Enum")) {
            Attributes = TypeAttributes.Public | TypeAttributes.Sealed,
        };
        en.Fields.Add(new FieldDefUser("value__", new FieldSig(cs.Int32),
            FieldAttributes.Public | FieldAttributes.SpecialName | FieldAttributes.RTSpecialName));
        mod.Types.Add(en);

        var other = new AssemblyRefUser("Other", new Version(1, 0, 0, 0));
        mod.ExportedTypes.Add(new ExportedTypeUser(mod, 0, "Fwd", "En", TypeAttributes.Public, other));

        var attr = new TypeDefUser("", "Attr", mod.CorLibTypes.GetTypeRef("System", "Attribute")) {
            Attributes = TypeAttributes.Public | TypeAttributes.BeforeFieldInit,
        };
        mod.Types.Add(attr);
        var uses = NewClass(mod, "", "Uses");
        mod.Types.Add(uses);

        var self = new ModuleRefUser(mod, mod.Name);
        var netmodule = new ModuleRefUser(mod, "other.netmodule");
        // TypeRefs for the attribute constructors to take, each with the
        // ResolutionScope to patch in, if dnlib cannot write it. dnlib folds
        // TypeRefs with equal names and scopes, so those start out with
        // distinct placeholder scopes.
        var placeholder = 0;
        TypeRef Ref(string ns, IResolutionScope scope) => new TypeRefUser(mod, ns, "En", scope);
        TypeRef Placeholder(string ns) => Ref(ns, new ModuleRefUser(mod, $"placeholder{++placeholder}"));
        var refs = new (TypeRef, uint?)[] {
            // Forwarded to another assembly; scoped to this module, or to
            // nothing.
            (Placeholder("Fwd"), ModuleScope),
            (Placeholder("Fwd"), 0),
            // Defined here; scoped to this module, to nothing, to a module
            // reference naming this module, and to one naming another.
            (Placeholder("Local"), ModuleScope),
            (Placeholder("Local"), 0),
            (Ref("Local", self), null),
            (Ref("Local", netmodule), null),
        };
        foreach (var (tr, _) in refs) {
            var ctor = Method(".ctor", MethodSig.CreateInstance(cs.Void, new ValueTypeSig(tr)), Ctor);
            attr.Methods.Add(ctor);
            // The value 5, written raw: dnlib would need to resolve the enum.
            uses.CustomAttributes.Add(new CustomAttribute(ctor, new byte[] { 1, 0, 5, 0, 0, 0, 0, 0 }));
        }

        return (mod, md => {
            foreach (var (tr, scope) in refs) {
                if (scope is uint s)
                    Patch(md.TablesHeap.TypeRefTable, md.GetRid(tr), r => new RawTypeRefRow(s, r.Name, r.Namespace));
            }
        });
    }

    // ResolutionScope coded index of Module rid 1.
    const uint ModuleScope = (1 << 2) | 0;

    static readonly byte[] Key = Convert.FromHexString(
        "0024000004800000940000000602000000240000525341310004000001000100" +
        "b5fc90e7027f67871e773a8fde8938c81dd402ba65b9201d60593e96c492651e" +
        "889cc13f1415ebb53fac1131ae0bd333c5ee6021672d9718ea31a8aebd0da007" +
        "2f25d87dba6fc90ffd598ed4da35e44c398c454307e8e33b8426143daec9f596" +
        "836f97c8f74750e5975c64e2189f45def46b2a2b1247adc3652bf5c308055da9");

    // A PublicKey flag that disagrees with the key. Cecil
    // derives HasPublicKey from the key, for the assembly and its
    // references.
    static (ModuleDef, Action<Metadata>) PublicKeys(string name, bool flag, byte[] key) {
        var mod = NewModule(name);
        var cs = mod.CorLibTypes;
        var asm = mod.Assembly;
        asm.PublicKey = new PublicKey(Key);
        // References with the flag and a full key, with the flag and no key,
        // and with a token.
        var withKey = new AssemblyRefUser("WithKey", new Version(1, 0, 0, 0), new PublicKey(Key));
        var flagNoKey = new AssemblyRefUser("FlagNoKey", new Version(1, 0, 0, 0), new PublicKey(Key));
        var token = new AssemblyRefUser("Token", new Version(1, 0, 0, 0), new PublicKeyToken("0123456789abcdef"));
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        foreach (var r in new[] { withKey, flagNoKey, token })
            c.Fields.Add(new FieldDefUser(r.Name, new FieldSig(new ClassSig(new TypeRefUser(mod, "", "T", r))), FieldAttributes.Public));

        return (mod, md => {
            const uint PublicKeyFlag = (uint)AssemblyAttributes.PublicKey;
            var blob = md.BlobHeap.Add(key);
            Patch(md.TablesHeap.AssemblyTable, 1, r => new RawAssemblyRow(r.HashAlgId,
                r.MajorVersion, r.MinorVersion, r.BuildNumber, r.RevisionNumber,
                flag ? r.Flags | PublicKeyFlag : r.Flags & ~PublicKeyFlag, blob, r.Name, r.Locale));
            var empty = md.BlobHeap.Add(Array.Empty<byte>());
            Patch(md.TablesHeap.AssemblyRefTable, md.GetRid(flagNoKey), r => new RawAssemblyRefRow(
                r.MajorVersion, r.MinorVersion, r.BuildNumber, r.RevisionNumber,
                r.Flags | PublicKeyFlag, empty, r.Name, r.Locale, r.HashValue));
        });
    }

    static AssemblyRefUser GapRef(string name) => new AssemblyRefUser($"gap-{name}", new Version(1, 0, 0, 0));

    // The definitions gap-resolve.dll's references resolve to (cil-dump's
    // [resolve] section probes the fixtures directory), and attribute
    // arguments naming nested enums.
    static (ModuleDef, Action<Metadata>) ResolveLib() {
        var mod = NewModule("resolve-lib");
        var cs = mod.CorLibTypes;
        FieldDef Field(TypeDef t, string name, TypeSig type) {
            var f = new FieldDefUser(name, new FieldSig(type), FieldAttributes.Public | FieldAttributes.Static);
            t.Fields.Add(f);
            return f;
        }
        MethodDef Add(TypeDef t, string name, MethodSig sig) {
            var m = Method(name, sig, sig.HasThis ? Instance : Static);
            t.Methods.Add(m);
            return m;
        }
        TypeDef Class(string name, ITypeDefOrRef baseType = null) {
            var t = NewClass(mod, "", name);
            if (baseType != null)
                t.BaseType = baseType;
            mod.Types.Add(t);
            return t;
        }

        // Cecil gives up on a type with no fields (or methods) before
        // walking its bases.
        var @base = Class("Base");
        Field(@base, "f", cs.Int32);
        Add(@base, "M", MethodSig.CreateStatic(cs.Void));
        Class("NoMembers", @base);
        var other = Class("Other", @base);
        Field(other, "g", cs.Int32);
        Add(other, "N", MethodSig.CreateStatic(cs.Void));
        // Its only field is deleted, so Cecil sees none.
        var deletedOnly = Class("DeletedOnly", @base);
        Field(deletedOnly, "_Deleted", cs.Int32).Attributes |= FieldAttributes.SpecialName;

        // Array and function-pointer shapes.
        var arrays = Class("Arrays");
        Field(arrays, "F2", new ArraySig(cs.Int32, 2));
        Field(arrays, "F1", new SZArraySig(cs.Int32));
        Add(arrays, "M2", MethodSig.CreateStatic(cs.Void, new ArraySig(cs.Int32, 2)));
        Add(arrays, "M1", MethodSig.CreateStatic(cs.Void, new SZArraySig(cs.Int32)));
        Add(arrays, "MB", MethodSig.CreateStatic(cs.Void, new ArraySig(cs.Int32, 1u, new uint[0], new[] { 0 })));
        var fnptrs = Class("FnPtrs");
        Add(fnptrs, "P", MethodSig.CreateStatic(cs.Void, new FnPtrSig(MethodSig.CreateStatic(cs.Void, cs.Int32))));
        Add(fnptrs, "Q", MethodSig.CreateStatic(cs.Void, new FnPtrSig(MethodSig.CreateInstance(cs.Void, cs.Int32))));

        // Nested types, one of them with a namespace; and a
        // forwarder for a name Outer does not nest.
        var outer = Class("Outer");
        foreach (var (ns, name) in new[] { ("", "Inner"), ("NsA", "Inner2") }) {
            var nested = new TypeDefUser(ns, name, cs.Object.TypeDefOrRef) {
                Attributes = TypeAttributes.NestedPublic,
            };
            outer.NestedTypes.Add(nested);
        }
        mod.ExportedTypes.Add(new ExportedTypeUser(mod, 0, "", "Missing", TypeAttributes.Forwarder, GapRef("resolve")));
        // Nested types in attribute arguments: an enum nested with a namespace, named
        // by a TypeRef without the namespace (no match), and by a reflection
        // name with it (a match).
        var en = new TypeDefUser("NsA", "E", cs.GetTypeRef("System", "Enum")) {
            Attributes = TypeAttributes.NestedPublic | TypeAttributes.Sealed,
        };
        en.Fields.Add(new FieldDefUser("value__", new FieldSig(cs.Int32),
            FieldAttributes.Public | FieldAttributes.SpecialName | FieldAttributes.RTSpecialName));
        outer.NestedTypes.Add(en);
        var attr = new TypeDefUser("", "Attr", cs.GetTypeRef("System", "Attribute")) {
            Attributes = TypeAttributes.Public | TypeAttributes.BeforeFieldInit,
        };
        mod.Types.Add(attr);
        var outerRef = new TypeRefUser(mod, "", "Outer", new ModuleRefUser(mod, "placeholder"));
        var byRef = Method(".ctor", MethodSig.CreateInstance(cs.Void, new ValueTypeSig(new TypeRefUser(mod, "", "E", outerRef))), Ctor);
        var boxed = Method(".ctor", MethodSig.CreateInstance(cs.Void, cs.Object), Ctor);
        attr.Methods.Add(byRef);
        attr.Methods.Add(boxed);
        var attributed = Class("Attributed");
        attributed.CustomAttributes.Add(new CustomAttribute(byRef, new byte[] { 1, 0, 5, 0, 0, 0, 0, 0 }));
        var enumName = Encoding.UTF8.GetBytes("Outer+NsA.E");
        var blob = new List<byte> { 1, 0, 0x51, 0x55, (byte)enumName.Length };
        blob.AddRange(enumName);
        blob.AddRange(new byte[] { 5, 0, 0, 0, 0, 0 });
        attributed.CustomAttributes.Add(new CustomAttribute(boxed, blob.ToArray()));

        // Two top-level types named Dup. Cecil's name lookup finds the
        // last, also when resolving a reference to the first.
        var dup1 = Class("Dup");
        Field(dup1, "first", cs.Int32);
        var dup2 = Class("Dup");
        Field(dup2, "second", cs.Int32);
        var uses = Class("Uses");
        Add(uses, "Refs", MethodSig.CreateStatic(cs.Void)).Body.Instructions.Insert(0,
            OpCodes.Ldtoken.ToInstruction(new TypeSpecUser(new SZArraySig(new ClassSig(dup1)))));

        return (mod, md => Patch(md.TablesHeap.TypeRefTable, md.GetRid(outerRef), r => new RawTypeRefRow(ModuleScope, r.Name, r.Namespace)));
    }

    static TypeDefUser NewEnum(ModuleDef mod, string ns, string name, TypeSig underlying,
        TypeAttributes visibility = TypeAttributes.Public) {
        var en = new TypeDefUser(ns, name, mod.CorLibTypes.GetTypeRef("System", "Enum")) {
            Attributes = visibility | TypeAttributes.Sealed,
        };
        en.Fields.Add(new FieldDefUser("value__", new FieldSig(underlying),
            FieldAttributes.Public | FieldAttributes.SpecialName | FieldAttributes.RTSpecialName));
        return en;
    }

    // The target of gap-enum-lib.dll's forwarder.
    static (ModuleDef, Action<Metadata>) EnumMoved() {
        var mod = NewModule("enum-moved");
        mod.Types.Add(NewEnum(mod, "Lib", "Moved", mod.CorLibTypes.SByte));
        return (mod, _ => { });
    }

    // Enums that gap-enums.dll's attribute arguments name from another
    // assembly, and the shapes Cecil reads them through: an underlying type
    // that is a TypeRef to a primitive or to another assembly's enum, a
    // nested enum, a struct, and forwarders to an assembly in the fixtures
    // directory and to one that is not.
    static (ModuleDef, Action<Metadata>) EnumLib() {
        var mod = NewModule("enum-lib");
        var cs = mod.CorLibTypes;
        mod.Types.Add(NewEnum(mod, "Lib", "Small", cs.Byte));
        mod.Types.Add(NewEnum(mod, "Lib", "Wide", cs.Int64));
        mod.Types.Add(NewEnum(mod, "Lib", "ByRef", new ValueTypeSig(cs.GetTypeRef("System", "Int16"))));
        mod.Types.Add(NewEnum(mod, "Lib", "Chained",
            new ValueTypeSig(new TypeRefUser(mod, "Lib", "Moved", GapRef("enum-moved")))));
        var outer = NewClass(mod, "Lib", "Outer");
        outer.NestedTypes.Add(NewEnum(mod, "", "Inner", cs.UInt16, TypeAttributes.NestedPublic));
        mod.Types.Add(outer);
        var notEnum = new TypeDefUser("Lib", "NotEnum", cs.GetTypeRef("System", "ValueType")) {
            Attributes = TypeAttributes.Public | TypeAttributes.Sealed,
        };
        notEnum.Fields.Add(new FieldDefUser("value__", new FieldSig(cs.Int32), FieldAttributes.Public));
        mod.Types.Add(notEnum);
        mod.ExportedTypes.Add(new ExportedTypeUser(mod, 0, "Lib", "Moved", TypeAttributes.Forwarder, GapRef("enum-moved")));
        mod.ExportedTypes.Add(new ExportedTypeUser(mod, 0, "Lib", "Gone", TypeAttributes.Forwarder, GapRef("absent")));
        return (mod, _ => { });
    }

    // Attribute arguments whose enum types live in gap-enum-lib.dll: named
    // by TypeRefs in constructor signatures, and boxed with their types
    // named in the blob (0x55), with the assembly part matching the
    // reference, naming it otherwise, missing, or malformed. Without
    // assembly resolution none of them read; with it, the arguments read
    // as far as the fixtures directory has the types.
    static (ModuleDef, Action<Metadata>) Enums() {
        var mod = NewModule("enums");
        var cs = mod.CorLibTypes;
        var lib = GapRef("enum-lib");
        TypeRef Ref(string name, IResolutionScope scope = null) => new TypeRefUser(mod, "Lib", name, scope ?? lib);
        var attr = new TypeDefUser("", "Attr", cs.GetTypeRef("System", "Attribute")) {
            Attributes = TypeAttributes.Public | TypeAttributes.BeforeFieldInit,
        };
        mod.Types.Add(attr);
        var uses = NewClass(mod, "", "Uses");
        mod.Types.Add(uses);

        var refs = new (ITypeDefOrRef, byte[])[] {
            (Ref("Small"), new byte[] { 7 }),
            (Ref("Wide"), new byte[] { 1, 2, 3, 4, 5, 6, 7, 8 }),
            (Ref("ByRef"), new byte[] { 1, 2 }),
            (Ref("Chained"), new byte[] { 0xfe }),
            (new TypeRefUser(mod, "", "Inner", Ref("Outer")), new byte[] { 1, 2 }),
            (Ref("NotEnum"), new byte[] { 1, 2, 3, 4 }),
            (Ref("Moved"), new byte[] { 0xfd }),
            (Ref("Gone"), new byte[] { 1, 2, 3, 4 }),
            (Ref("Nowhere"), new byte[] { 1, 2, 3, 4 }),
            (Ref("X", new AssemblyRefUser("gap-absent", new Version(1, 0, 0, 0))), new byte[] { 1, 2, 3, 4 }),
        };
        foreach (var (type, value) in refs) {
            var ctor = Method(".ctor", MethodSig.CreateInstance(cs.Void, new ValueTypeSig(type)), Ctor);
            attr.Methods.Add(ctor);
            var blob = new List<byte> { 1, 0 };
            blob.AddRange(value);
            blob.AddRange(new byte[] { 0, 0 });
            uses.CustomAttributes.Add(new CustomAttribute(ctor, blob.ToArray()));
        }

        var boxed = Method(".ctor", MethodSig.CreateInstance(cs.Void, cs.Object), Ctor);
        attr.Methods.Add(boxed);
        var names = new (string, byte[])[] {
            ("Lib.Small, gap-enum-lib, Version=1.0.0.0, Culture=neutral, PublicKeyToken=null", new byte[] { 7 }),
            // Version 1.0 is 1.0.0.0, so this matches the reference too.
            ("Lib.Small, gap-enum-lib, Version=1.0", new byte[] { 7 }),
            // No reference matches version 0.0.0.0: the parsed name scopes it.
            ("Lib.Small, gap-enum-lib", new byte[] { 7 }),
            ("Lib.Wide, gap-enum-lib, Version=1.0.0.0", new byte[] { 1, 2, 3, 4, 5, 6, 7, 8 }),
            ("Lib.Outer+Inner, gap-enum-lib, Version=1.0.0.0", new byte[] { 1, 2 }),
            ("Lib.Chained, gap-enum-lib, Version=1.0.0.0", new byte[] { 0xfe }),
            ("Lib.Moved, gap-enum-lib, Version=1.0.0.0", new byte[] { 0xfd }),
            ("Lib.NotEnum, gap-enum-lib, Version=1.0.0.0", new byte[] { 1, 2, 3, 4 }),
            ("Lib.Nowhere, gap-enum-lib, Version=1.0.0.0", new byte[] { 1, 2, 3, 4 }),
            // No assembly part: the core library, mscorlib, scopes it.
            ("Lib.Small", new byte[] { 7 }),
            ("Lib.Small, gap-enum-lib, Version=x", new byte[] { 7 }),
            ("Lib.Small, gap-enum-lib, Culture", new byte[] { 7 }),
        };
        foreach (var (name, value) in names) {
            var bytes = Encoding.UTF8.GetBytes(name);
            if (bytes.Length >= 0x80)
                throw new InvalidOperationException($"name too long for a 1-byte length: {name}");
            var blob = new List<byte> { 1, 0, 0x51, 0x55, (byte)bytes.Length };
            blob.AddRange(bytes);
            blob.AddRange(value);
            blob.AddRange(new byte[] { 0, 0 });
            uses.CustomAttributes.Add(new CustomAttribute(boxed, blob.ToArray()));
        }
        return (mod, _ => { });
    }

    // References into gap-resolve-lib.dll that Cecil resolves differently
    // from a name-only lookup.
    static (ModuleDef, Action<Metadata>) Resolve() {
        var mod = NewModule("resolve");
        var cs = mod.CorLibTypes;
        var lib = GapRef("resolve-lib");
        TypeRef Ref(string name, IResolutionScope scope = null, string ns = "") => new TypeRefUser(mod, ns, name, scope ?? lib);
        var body = new List<Instruction>();
        void Load(string type, string name, TypeSig fieldType) =>
            body.Add(OpCodes.Ldsfld.ToInstruction(new MemberRefUser(mod, name, new FieldSig(fieldType), Ref(type))));
        void Call(string type, string name, MethodSig sig) =>
            body.Add(OpCodes.Call.ToInstruction(new MemberRefUser(mod, name, sig, Ref(type))));
        void Token(ITypeDefOrRef type) => body.Add(OpCodes.Ldtoken.ToInstruction(type));

        // Types with no fields or methods of their own: no base-type walk.
        foreach (var type in new[] { "NoMembers", "Other", "DeletedOnly" }) {
            Load(type, "f", cs.Int32);
            Call(type, "M", MethodSig.CreateStatic(cs.Void));
        }
        // Arrays: rank decides; bounds and SZARRAY-vs-ARRAY do not.
        var bounded2 = new ArraySig(cs.Int32, 2u, new uint[0], new[] { 0, 0 });
        var bounded1 = new ArraySig(cs.Int32, 1u, new uint[0], new[] { 0 });
        var unbounded1 = new ArraySig(cs.Int32, 1);
        Load("Arrays", "F2", bounded2);
        Load("Arrays", "F1", bounded1);
        Call("Arrays", "M2", MethodSig.CreateStatic(cs.Void, bounded2));
        Call("Arrays", "M2", MethodSig.CreateStatic(cs.Void, new SZArraySig(cs.Int32)));
        Call("Arrays", "M1", MethodSig.CreateStatic(cs.Void, bounded1));
        Call("Arrays", "M1", MethodSig.CreateStatic(cs.Void, unbounded1));
        Call("Arrays", "MB", MethodSig.CreateStatic(cs.Void, new SZArraySig(cs.Int32)));
        // Function pointers: calling convention and HasThis decide.
        Call("FnPtrs", "P", MethodSig.CreateStatic(cs.Void, new FnPtrSig(MethodSig.CreateStatic(cs.Void, cs.Int32))));
        Call("FnPtrs", "P", MethodSig.CreateStatic(cs.Void, new FnPtrSig(new MethodSig(CallingConvention.C, 0, cs.Void, cs.Int32))));
        Call("FnPtrs", "P", MethodSig.CreateStatic(cs.Void, new FnPtrSig(MethodSig.CreateInstance(cs.Void, cs.Int32))));
        Call("FnPtrs", "Q", MethodSig.CreateStatic(cs.Void, new FnPtrSig(MethodSig.CreateInstance(cs.Void, cs.Int32))));
        // Nested types match on namespace and name.
        var outer = Ref("Outer");
        foreach (var (ns, name) in new[] { ("", "Inner"), ("NsA", "Inner2"), ("", "Inner2"), ("NsB", "Inner") })
            Token(new TypeRefUser(mod, ns, name, outer));
        // Outer does not nest Missing; the library forwards that name
        // back here.
        Token(new TypeRefUser(mod, "", "Missing", outer));
        var missing = NewClass(mod, "", "Missing");
        mod.Types.Add(missing);
        // Dup: the reference to the first resolves to the last.
        Token(Ref("Dup"));
        Load("Dup", "first", cs.Int32);
        Load("Dup", "second", cs.Int32);
        // A module reference naming this module.
        Token(Ref("Missing", new ModuleRefUser(mod, mod.Name)));

        var refs = NewClass(mod, "", "Refs");
        mod.Types.Add(refs);
        // The body only holds the references; it is never run.
        var use = Method("Use", MethodSig.CreateStatic(cs.Void), Static, body.ToArray());
        use.Body.KeepOldMaxStack = true;
        use.Body.MaxStack = 8;
        refs.Methods.Add(use);
        return (mod, _ => { });
    }

    // Base types written as TypeSpecs. Cecil reads a TypeSpec
    // of a plain class or valuetype as that type, and names a generic
    // instance after its element type, so those count as value types or
    // enums; a modified or array type's name has a suffix, so it does not.
    static (ModuleDef, Action<Metadata>) BaseSpecs() {
        var mod = NewModule("base-specs");
        var cs = mod.CorLibTypes;
        var valueType = cs.GetTypeRef("System", "ValueType");
        var @enum = cs.GetTypeRef("System", "Enum");
        var isConst = new TypeRefUser(mod, "System.Runtime.CompilerServices", "IsConst", cs.AssemblyRef);
        var bases = new (string, TypeSig)[] {
            ("ClassSpec", new ClassSig(valueType)),
            ("ValueTypeSpec", new ValueTypeSig(valueType)),
            ("EnumSpec", new ClassSig(@enum)),
            ("GenericSpec", new GenericInstSig(new ClassSig(valueType), cs.Int32)),
            ("ModReqSpec", new CModReqdSig(isConst, new ClassSig(valueType))),
            ("ArraySpec", new SZArraySig(new ClassSig(valueType))),
        };
        foreach (var (name, sig) in bases) {
            var t = new TypeDefUser("", name, new TypeSpecUser(sig)) {
                Attributes = TypeAttributes.Public | TypeAttributes.Sealed,
            };
            mod.Types.Add(t);
        }
        return (mod, _ => { });
    }

    // SpecialName members named _Deleted, which edit-and-continue
    // leaves behind. Cecil leaves them out of their type's members.
    static (ModuleDef, Action<Metadata>) Deleted() {
        var mod = NewModule("deleted");
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        c.Fields.Add(new FieldDefUser("_Deleted", new FieldSig(cs.Int32), FieldAttributes.Public | FieldAttributes.SpecialName));
        c.Fields.Add(new FieldDefUser("kept", new FieldSig(cs.Int32), FieldAttributes.Public));
        var get = Method("get_P", MethodSig.CreateInstance(cs.Int32), Accessor, OpCodes.Ldc_I4_0.ToInstruction());
        c.Methods.Add(get);
        // Last of its type: Cecil skips a deleted method before reading the
        // rest of its row, so it misreads the type's later methods.
        c.Methods.Add(Method("_Deleted", MethodSig.CreateInstance(cs.Int32), Accessor, OpCodes.Ldc_I4_0.ToInstruction()));
        c.Properties.Add(new PropertyDefUser("_Deleted", PropertySig.CreateInstance(cs.Int32), PropertyAttributes.SpecialName) { GetMethod = get });
        c.Properties.Add(new PropertyDefUser("P", PropertySig.CreateInstance(cs.Int32)) { GetMethod = get });
        var handler = new TypeRefUser(mod, "System", "EventHandler", cs.AssemblyRef);
        c.Events.Add(new EventDefUser("_Deleted", handler, EventAttributes.SpecialName));
        // Without SpecialName, the name alone does not delete a member.
        var d = NewClass(mod, "", "D");
        mod.Types.Add(d);
        d.Fields.Add(new FieldDefUser("_Deleted", new FieldSig(cs.Int32), FieldAttributes.Public));
        d.Methods.Add(Method("_Deleted", MethodSig.CreateStatic(cs.Void), Static));
        return (mod, _ => { });
    }

    static UTF8String Bytes(params byte[] b) => new UTF8String(b);

    // #Strings entries that are not valid UTF-8. Cecil decodes them with
    // Encoding.UTF8, which turns each maximal invalid subsequence into
    // U+FFFD.
    static (ModuleDef, Action<Metadata>) Utf8Names() {
        var mod = NewModule("utf8-names");
        var cs = mod.CorLibTypes;
        // A namespace with a byte that never starts a sequence, and a name
        // with an encoded surrogate (ED A0 80).
        var t = new TypeDefUser(Bytes(0x4e, 0xff), Bytes(0x54, 0xed, 0xa0, 0x80, 0x78), cs.Object.TypeDefOrRef) {
            Attributes = TypeAttributes.Public | TypeAttributes.BeforeFieldInit,
        };
        mod.Types.Add(t);
        // A field named with an overlong encoding of NUL, of a type whose
        // TypeRef name is a sequence cut short by the end of the string.
        var missing = new TypeRefUser(mod, "System", Bytes(0x58, 0xe2, 0x82), cs.AssemblyRef);
        t.Fields.Add(new FieldDefUser(Bytes(0xc0, 0x80), new FieldSig(new ClassSig(missing)), FieldAttributes.Public));
        t.Methods.Add(new MethodDefUser(Bytes(0x4d, 0x80), MethodSig.CreateStatic(cs.Void), Impl, Static) {
            Body = new CilBody { Instructions = { OpCodes.Ret.ToInstruction() } },
        });
        // A name whose index points into the middle of a string: past the C3
        // of the e-acute in "Caf\u00e9", at its continuation byte A9. Create,
        // unlike Add, gives the string its final offset at once.
        var mid = NewClass(mod, "", "Mid");
        mod.Types.Add(mid);
        return (mod, md => {
            var cafe = md.StringsHeap.Create(new UTF8String("Caf\u00e9"));
            Patch(md.TablesHeap.TypeDefTable, md.GetRid(mid), r =>
                new RawTypeDefRow(r.Flags, cafe + 4, r.Namespace, r.Extends, r.FieldList, r.MethodList));
        });
    }

    // Stream headers and a version string that Cecil reads where a strict
    // reader rejects the image: a junk stream that starts past the end of
    // the file, a #US whose size runs past it (Cecil reads to the end of
    // the file), a stream name with a byte that is not ASCII, and a version
    // string ending in one. Cecil decodes names and the version byte by
    // byte, as Latin-1.
    static void Streams(byte[] image) {
        var root = MetadataRoot(image);
        foreach (var (at, name) in StreamHeaders(image, root)) {
            switch (name) {
            case "#XS":
                WriteU32(image, at, 0x7000_0000);
                WriteU32(image, at + 4, 0x100);
                break;
            case "#US":
                WriteU32(image, at + 4, 0x0100_0000);
                break;
            case "#YS":
                image[at + 9] = 0xff;
                break;
            }
        }
        // "v4.0.30319" and its padding: end it with an e-acute.
        var version = Encoding.ASCII.GetString(image, root + 16, 10);
        if (version != "v4.0.30319" || image[root + 16 + 10] != 0)
            throw new InvalidOperationException($"unexpected version {version}");
        image[root + 16 + 10] = 0xe9;
    }

    // TypeRefs whose ResolutionScope is a TypeRef rid of 0 or past the
    // TypeRef table. Cecil's GetTypeReference finds no row there, so the
    // reference has no declaring type, is scoped to this module, and is
    // named by its own namespace and name.
    static (ModuleDef, Action<Metadata>) TypeRefScopes() {
        var mod = NewModule("typeref-scopes");
        var cs = mod.CorLibTypes;
        var lib = new AssemblyRefUser("Lib", new Version(1, 0, 0, 0));
        var outer = new TypeRefUser(mod, "Lib", "Outer", lib);
        var zero = new TypeRefUser(mod, "", "InnerZero", outer);
        var past = new TypeRefUser(mod, "Ns", "InnerPast", outer);
        var nested = new TypeRefUser(mod, "", "Deeper", past);

        var uses = new TypeDefUser("", "Uses", past) {
            Attributes = TypeAttributes.Public | TypeAttributes.BeforeFieldInit,
        };
        mod.Types.Add(uses);
        foreach (var (name, tr) in new[] { ("zero", zero), ("past", past), ("nested", nested) })
            uses.Fields.Add(new FieldDefUser(name, new FieldSig(new ClassSig(tr)), FieldAttributes.Public));
        uses.Methods.Add(Method("Tokens", MethodSig.CreateStatic(cs.Void), Static,
            OpCodes.Ldtoken.ToInstruction(zero),
            OpCodes.Pop.ToInstruction(),
            OpCodes.Ldtoken.ToInstruction(past),
            OpCodes.Pop.ToInstruction(),
            OpCodes.Ldtoken.ToInstruction(nested),
            OpCodes.Pop.ToInstruction()));

        // TypeRef's tag in ResolutionScope is 3; 0x3fff is the largest rid
        // a 2-byte coded index holds.
        return (mod, md => {
            Patch(md.TablesHeap.TypeRefTable, md.GetRid(zero), r => new RawTypeRefRow((0 << 2) | 3, r.Name, r.Namespace));
            Patch(md.TablesHeap.TypeRefTable, md.GetRid(past), r => new RawTypeRefRow((0x3fff << 2) | 3, r.Name, r.Namespace));
        });
    }

    // Heap indexes past their heaps, and blobs whose length runs past the
    // #Blob heap. Cecil reads an out-of-range #Strings index as "", and a
    // #GUID index past the heap as the zero GUID. It reads a raw blob (a
    // public key) whose length runs past the heap as empty, and a signature
    // from the blob's start through the rest of the heap.
    static (ModuleDef, Action<Metadata>) HeapIndexes() {
        var mod = NewModule("heap-indexes");
        var cs = mod.CorLibTypes;
        mod.Assembly.PublicKey = new PublicKey(Key);
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        var unnamed = new FieldDefUser("Unnamed", new FieldSig(cs.Int32), FieldAttributes.Public);
        var sig = new FieldDefUser("Sig", new FieldSig(cs.String), FieldAttributes.Public);
        var constant = new FieldDefUser("K", new FieldSig(cs.Int32),
            FieldAttributes.Public | FieldAttributes.Static | FieldAttributes.Literal | FieldAttributes.HasDefault) {
            Constant = new ConstantUser(7),
        };
        foreach (var f in new[] { unnamed, sig, constant })
            c.Fields.Add(f);
        var obsolete = new TypeRefUser(mod, "System", "ObsoleteAttribute", cs.AssemblyRef);
        var ctor = new MemberRefUser(mod, ".ctor", MethodSig.CreateInstance(cs.Void), obsolete);
        c.CustomAttributes.Add(new CustomAttribute(ctor));

        return (mod, md => {
            Patch(md.TablesHeap.ModuleTable, 1, r => new RawModuleRow(r.Generation, r.Name, 9, r.EncId, r.EncBaseId));
            Patch(md.TablesHeap.FieldTable, md.GetRid(unnamed), r => new RawFieldRow(r.Flags, 0xfff0, r.Signature));
            // Each blob's first two bytes are the length 0x3fff, which the
            // index one past the blob's own one-byte length reads.
            uint Overlong(params byte[] rest) {
                var blob = new byte[] { 0xbf, 0xff }.Concat(rest).ToArray();
                return md.BlobHeap.Add(blob) + 1;
            }
            Patch(md.TablesHeap.FieldTable, md.GetRid(sig), r =>
                new RawFieldRow(r.Flags, r.Name, Overlong(0x06, 0x08)));
            var k = md.TablesHeap.ConstantTable;
            for (uint rid = 1; rid <= k.Rows; rid++)
                Patch(k, rid, r => new RawConstantRow(r.Type, r.Padding, r.Parent, Overlong(0x2a, 0, 0, 0)));
            var ca = md.TablesHeap.CustomAttributeTable;
            for (uint rid = 1; rid <= ca.Rows; rid++)
                Patch(ca, rid, r => new RawCustomAttributeRow(r.Parent, r.Type, Overlong(0x01, 0x00, 0x00, 0x00)));
            Patch(md.TablesHeap.AssemblyTable, 1, r => new RawAssemblyRow(r.HashAlgId,
                r.MajorVersion, r.MinorVersion, r.BuildNumber, r.RevisionNumber,
                r.Flags, Overlong(1, 2, 3, 4), r.Name, r.Locale));
        });
    }

    // Methods with two try/finally clauses each, whose exception-handling
    // section EhSectionHeaders rewrites. Each body starts with ldc.i4 of its
    // own marker, which finds it in the written image.
    static readonly (string, uint)[] EhMethods = {
        ("Kept", 0x5ec7_0001),
        ("Negative", 0x5ec7_0002),
        ("MinusOne", 0x5ec7_0003),
        ("NegativeThenSmall", 0x5ec7_0004),
        ("Kind3", 0x5ec7_0005),
        ("Kind5", 0x5ec7_0006),
        ("Kind6", 0x5ec7_0007),
        ("Kind7", 0x5ec7_0008),
    };

    static ModuleDef EhSections() {
        var mod = NewModule("eh-sections");
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        foreach (var (name, marker) in EhMethods) {
            var m = new MethodDefUser(name, MethodSig.CreateStatic(cs.Void), Impl, Static) { Body = new CilBody() };
            var ins = m.Body.Instructions;
            var ret = OpCodes.Ret.ToInstruction();
            var try2 = OpCodes.Nop.ToInstruction();
            ins.Add(OpCodes.Ldc_I4.ToInstruction((int)marker));
            ins.Add(OpCodes.Pop.ToInstruction());
            for (int i = 0; i < 2; i++) {
                var tryStart = i == 0 ? OpCodes.Nop.ToInstruction() : try2;
                var handler = OpCodes.Endfinally.ToInstruction();
                ins.Add(tryStart);
                ins.Add(OpCodes.Leave_S.ToInstruction(i == 0 ? try2 : ret));
                ins.Add(handler);
                m.Body.ExceptionHandlers.Add(new ExceptionHandler(ExceptionHandlerType.Finally) {
                    TryStart = tryStart, TryEnd = handler,
                    HandlerStart = handler, HandlerEnd = i == 0 ? try2 : ret,
                });
            }
            ins.Add(ret);
            c.Methods.Add(m);
        }
        return mod;
    }

    // A fat section's header is a flags byte and a 24-bit size, which Cecil
    // reads as one int32 and shifts right: a size of 0x800000 or more is
    // negative, and its clause count, size / 24, is too. Cecil reads no
    // clauses for it, and reads a following section (MoreSects) from right
    // after the 4-byte header. Kept is left as dnlib wrote it; Negative's
    // size is 0x800000 and MinusOne's 0xffffe8, a count of -1; and
    // NegativeThenSmall's 0x800000 header is followed by a small section
    // holding its first clause. KindN's first clause has kind N, which
    // Cecil's ExceptionHandlerType does not name; Cecil reads it as it reads
    // a finally clause, and CecilDump prints it as a fault.
    static void EhSectionHeaders(byte[] image) {
        const uint fat = 0x41, moreSects = 0x80;
        foreach (var (name, marker) in EhMethods) {
            var ldc = new byte[] { 0x20 }.Concat(BitConverter.GetBytes(marker)).ToArray();
            int code = IndexOf(image, ldc);
            if (code < 0 || IndexOf(image, ldc, code + 1) >= 0)
                throw new InvalidOperationException($"{name}: marker not found exactly once");
            // The 12-byte fat header precedes the code, and the section
            // follows it at the next 4-byte boundary.
            int header = code - 12;
            if ((image[header] & 0x3) != 0x3 || (image[header] & 0x8) == 0)
                throw new InvalidOperationException($"{name}: no fat header with sections");
            int section = (code + ReadI32(image, header + 4) + 3) & ~3;
            if (image[section] != 0x01 || image[section + 1] != 4 + 2 * 12)
                throw new InvalidOperationException($"{name}: expected a small section of two clauses");
            var first = image.Skip(section + 4).Take(12).ToArray();
            switch (name) {
            case "Negative":
                WriteU32(image, section, fat | 0x80_0000u << 8);
                break;
            case "MinusOne":
                WriteU32(image, section, fat | 0xff_ffe8u << 8);
                break;
            case "NegativeThenSmall":
                WriteU32(image, section, fat | moreSects | 0x80_0000u << 8);
                new byte[] { 0x01, 4 + 12, 0, 0 }.CopyTo(image, section + 4);
                first.CopyTo(image, section + 8);
                break;
            case "Kind3":
            case "Kind5":
            case "Kind6":
            case "Kind7":
                image[section + 4] = (byte)(name[4] - '0');
                break;
            }
        }
    }

    // The tokens OperandTokenRows writes over the operands of M, after its
    // marker, in order. Cecil's LookupToken gives null for rid 0, for a rid
    // past its table and for a table it does not look up, and ReadCallSite
    // for a StandAloneSig rid past its table; the Formatter and CecilDump
    // then print no operand.
    const uint OperandMarker = 0x70c3_0001;
    static readonly uint[] OperandTokenValues = {
        0x0200_0000, // TypeDef rid 0
        0x0200_ffff, // past each table
        0x0100_ffff,
        0x1b00_ffff,
        0x0400_ffff,
        0x0600_ffff,
        0x0a00_ffff,
        0x2b00_ffff,
        0x0000_0001, // Module
        0x2300_0001, // AssemblyRef
        0x7000_0001, // a user string
        0x0400_ffff, // ldsfld
        0x0600_ffff, // call
        0x1100_ffff, // calli
    };

    // M: ldc.i4 of the marker, then one ldtoken of C for each token above
    // but the last three, an ldsfld, a call, and a calli, each followed by
    // a pop where it leaves a value. N, after them, keeps an ldtoken of C.
    static ModuleDef OperandTokens() {
        var mod = NewModule("operand-tokens");
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        var f = new FieldDefUser("F", new FieldSig(cs.Int32), FieldAttributes.Public | FieldAttributes.Static);
        c.Fields.Add(f);
        var n = new MethodDefUser("N", MethodSig.CreateStatic(cs.Void), Impl, Static) { Body = new CilBody() };
        n.Body.Instructions.Add(OpCodes.Ldtoken.ToInstruction(c));
        n.Body.Instructions.Add(OpCodes.Pop.ToInstruction());
        n.Body.Instructions.Add(OpCodes.Ret.ToInstruction());
        var m = new MethodDefUser("M", MethodSig.CreateStatic(cs.Void), Impl, Static) { Body = new CilBody() };
        var ins = m.Body.Instructions;
        ins.Add(OpCodes.Ldc_I4.ToInstruction((int)OperandMarker));
        ins.Add(OpCodes.Pop.ToInstruction());
        for (int i = 0; i < OperandTokenValues.Length - 3; i++) {
            ins.Add(OpCodes.Ldtoken.ToInstruction(c));
            ins.Add(OpCodes.Pop.ToInstruction());
        }
        ins.Add(OpCodes.Ldsfld.ToInstruction(f));
        ins.Add(OpCodes.Pop.ToInstruction());
        ins.Add(OpCodes.Call.ToInstruction(n));
        ins.Add(OpCodes.Ldnull.ToInstruction());
        ins.Add(OpCodes.Calli.ToInstruction(MethodSig.CreateStatic(cs.Void)));
        ins.Add(OpCodes.Ret.ToInstruction());
        c.Methods.Add(m);
        c.Methods.Add(n);
        return mod;
    }

    static void OperandTokenRows(byte[] image) {
        var ldc = new byte[] { 0x20 }.Concat(BitConverter.GetBytes(OperandMarker)).ToArray();
        int at = IndexOf(image, ldc);
        if (at < 0 || IndexOf(image, ldc, at + 1) >= 0)
            throw new InvalidOperationException("M: marker not found exactly once");
        at += 5 + 1; // past ldc.i4 and pop
        int i = 0;
        foreach (var token in OperandTokenValues) {
            // ldtoken, ldsfld and call take one opcode byte; calli is
            // preceded by ldnull. Each is followed by pop but the last two.
            byte op = i < OperandTokenValues.Length - 3 ? (byte)0xd0
                : i == OperandTokenValues.Length - 3 ? (byte)0x7e
                : i == OperandTokenValues.Length - 2 ? (byte)0x28
                : (byte)0x29;
            if (op == 0x29) {
                if (image[at] != 0x14)
                    throw new InvalidOperationException("M: expected ldnull before calli");
                at++;
            }
            if (image[at] != op)
                throw new InvalidOperationException($"M: expected opcode 0x{op:x2} at operand {i}");
            WriteU32(image, at + 1, token);
            at += 5;
            if (op == 0xd0 || op == 0x7e) {
                if (image[at] != 0x26)
                    throw new InvalidOperationException($"M: expected pop after operand {i}");
                at++;
            }
            i++;
        }
    }

    // Methods with an int32 and a string local, whose local-variable token
    // LocalTokenValues rewrites. Cecil's ReadVariables reads the
    // StandAloneSig row of the token's rid whatever its table, and a rid
    // past that table gives no locals, so ldloc.0 and stloc.0 have no
    // operand. Each body starts with ldc.i4 of its own marker.
    static readonly (string, uint)[] LocalMethods = {
        ("Kept", 0x10ca_0001),
        ("TypeDefTable", 0x10ca_0002),
        ("PastTable", 0x10ca_0003),
        ("TypeDefPastTable", 0x10ca_0004),
    };

    static ModuleDef LocalTokens() {
        var mod = NewModule("local-tokens");
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        foreach (var (name, marker) in LocalMethods) {
            var m = new MethodDefUser(name, MethodSig.CreateStatic(cs.Void), Impl, Static) { Body = new CilBody() };
            var i = new Local(cs.Int32);
            var str = new Local(cs.String);
            m.Body.Variables.Add(i);
            m.Body.Variables.Add(str);
            var ins = m.Body.Instructions;
            ins.Add(OpCodes.Ldc_I4.ToInstruction((int)marker));
            ins.Add(OpCodes.Stloc.ToInstruction(i));
            ins.Add(OpCodes.Ldloc.ToInstruction(i));
            ins.Add(OpCodes.Pop.ToInstruction());
            ins.Add(OpCodes.Ldnull.ToInstruction());
            ins.Add(OpCodes.Stloc.ToInstruction(str));
            ins.Add(OpCodes.Ret.ToInstruction());
            c.Methods.Add(m);
        }
        return mod;
    }

    static void LocalTokenValues(byte[] image) {
        foreach (var (name, marker) in LocalMethods) {
            var ldc = new byte[] { 0x20 }.Concat(BitConverter.GetBytes(marker)).ToArray();
            int code = IndexOf(image, ldc);
            if (code < 0 || IndexOf(image, ldc, code + 1) >= 0)
                throw new InvalidOperationException($"{name}: marker not found exactly once");
            // The fat header's last field, right before the code.
            int token = code - 4;
            if ((image[code - 12] & 0x3) != 0x3 || image[token + 3] != 0x11)
                throw new InvalidOperationException($"{name}: no fat header with a StandAloneSig token");
            switch (name) {
            case "TypeDefTable":
                image[token + 3] = 0x02;
                break;
            case "PastTable":
                WriteU32(image, token, 0x1100_ffff);
                break;
            case "TypeDefPastTable":
                WriteU32(image, token, 0x0200_ffff);
                break;
            }
        }
    }

    // Methods that end in an unreachable ldc.i4 of OverrunOperand, whose
    // code size CodeSizeValues cuts so that the operand runs past it. Each
    // body starts with ldc.i4 of its own marker and has a local, so its
    // header is fat. Section has a try/finally.
    const int OverrunOperand = 0x0ff0_0ff0;
    static readonly (string, uint)[] CodeSizeMethods = {
        ("Overrun", 0xc0de_0001),
        ("OverrunThenSection", 0xc0de_0002),
    };

    static ModuleDef CodeSizes() {
        var mod = NewModule("code-sizes");
        var cs = mod.CorLibTypes;
        var c = NewClass(mod, "", "C");
        mod.Types.Add(c);
        foreach (var (name, marker) in CodeSizeMethods) {
            var m = new MethodDefUser(name, MethodSig.CreateStatic(cs.Void), Impl, Static) { Body = new CilBody() };
            m.Body.Variables.Add(new Local(cs.Int32));
            var ins = m.Body.Instructions;
            ins.Add(OpCodes.Ldc_I4.ToInstruction((int)marker));
            ins.Add(OpCodes.Pop.ToInstruction());
            var ret = OpCodes.Ret.ToInstruction();
            if (name == "OverrunThenSection") {
                var tryStart = OpCodes.Nop.ToInstruction();
                var handler = OpCodes.Endfinally.ToInstruction();
                ins.Add(tryStart);
                ins.Add(OpCodes.Leave_S.ToInstruction(ret));
                ins.Add(handler);
                m.Body.ExceptionHandlers.Add(new ExceptionHandler(ExceptionHandlerType.Finally) {
                    TryStart = tryStart, TryEnd = handler,
                    HandlerStart = handler, HandlerEnd = ret,
                });
            }
            ins.Add(ret);
            ins.Add(OpCodes.Ldc_I4.ToInstruction(OverrunOperand));
            c.Methods.Add(m);
        }
        return mod;
    }

    // Cecil's CodeReader reads the last operand from the bytes after the
    // code, stops there, and aligns from the end of that operand to read
    // the sections. Overrun's code size ends 2 bytes into the operand.
    // OverrunThenSection's ends at the 4-byte boundary before the code's
    // real end (its operand's start, if the end is on one), so a reader
    // that aligns from the cut size reads the section 4 bytes early.
    static void CodeSizeValues(byte[] image) {
        var last = new byte[] { 0x20 }.Concat(BitConverter.GetBytes(OverrunOperand)).ToArray();
        foreach (var (name, marker) in CodeSizeMethods) {
            var ldc = new byte[] { 0x20 }.Concat(BitConverter.GetBytes(marker)).ToArray();
            int code = IndexOf(image, ldc);
            if (code < 0 || IndexOf(image, ldc, code + 1) >= 0)
                throw new InvalidOperationException($"{name}: marker not found exactly once");
            int header = code - 12;
            if ((image[header] & 0x3) != 0x3)
                throw new InvalidOperationException($"{name}: no fat header");
            int end = code + ReadI32(image, header + 4);
            if (!image.AsSpan(end - 5, 5).SequenceEqual(last))
                throw new InvalidOperationException($"{name}: code does not end in the overrun ldc.i4");
            int cut = name == "Overrun" ? 2 : end % 4 == 0 ? 4 : end % 4;
            WriteU32(image, header + 4, (uint)(end - cut - code));
        }
    }

    static int IndexOf(byte[] image, byte[] pattern, int from = 0) {
        for (int i = from; i + pattern.Length <= image.Length; i++)
            if (image.AsSpan(i, pattern.Length).SequenceEqual(pattern))
                return i;
        return -1;
    }

    // The file offset of the metadata root, from the PE headers.
    static int MetadataRoot(byte[] image) {
        int pe = ReadI32(image, 0x3c);
        int sections = BitConverter.ToUInt16(image, pe + 6);
        int optional = pe + 24;
        int optionalSize = BitConverter.ToUInt16(image, pe + 20);
        bool pe32Plus = BitConverter.ToUInt16(image, optional) == 0x20b;
        int cli = RvaToOffset(image, ReadI32(image, optional + (pe32Plus ? 224 : 208)), optional + optionalSize, sections);
        return RvaToOffset(image, ReadI32(image, cli + 8), optional + optionalSize, sections);
    }

    static int RvaToOffset(byte[] image, int rva, int sectionTable, int sections) {
        for (int i = 0; i < sections; i++) {
            int at = sectionTable + 40 * i;
            int va = ReadI32(image, at + 12), size = ReadI32(image, at + 16), raw = ReadI32(image, at + 20);
            if (rva >= va && rva < va + size)
                return rva - va + raw;
        }
        throw new InvalidOperationException($"rva {rva:x} outside any section");
    }

    // Each stream header's file offset and name.
    static List<(int, string)> StreamHeaders(byte[] image, int root) {
        var headers = new List<(int, string)>();
        int at = root + 16 + ReadI32(image, root + 12) + 2;
        int count = BitConverter.ToUInt16(image, at);
        at += 2;
        for (int i = 0; i < count; i++) {
            int name = at + 8, end = Array.IndexOf(image, (byte)0, name);
            headers.Add((at, Encoding.ASCII.GetString(image, name, end - name)));
            at = name + ((end - name + 4) & ~3);
        }
        return headers;
    }

    static int ReadI32(byte[] image, int at) => BitConverter.ToInt32(image, at);

    static void WriteU32(byte[] image, int at, uint value) => BitConverter.GetBytes(value).CopyTo(image, at);
}
