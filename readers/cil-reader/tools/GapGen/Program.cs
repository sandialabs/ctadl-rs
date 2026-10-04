// GapGen <outdir>: writes gap-<name>.dll, small modules with metadata shapes
// that neither Roslyn nor ilasm emit but obfuscators and other writers do,
// for comparing cil-reader against Cecil.
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
        return 0;
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
}
