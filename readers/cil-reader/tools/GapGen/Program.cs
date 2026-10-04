// GapGen <outdir>: writes gap-<name>.dll, small modules with metadata shapes
// that neither Roslyn nor ilasm emit but obfuscators and other writers do,
// for comparing cil-reader against Cecil (see readers/cil-reader/gaps.md).
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

    // gaps.md A3: a NestedClass row for a type whose visibility is not
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

    // gaps.md A4: generic parameters with empty names, as obfuscators leave
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

    // gaps.md A12: property signatures whose parameters are junk. Cecil
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

    // gaps.md A9: custom-attribute enum arguments whose TypeRef is scoped to
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

    // gaps.md A15: a PublicKey flag that disagrees with the key. Cecil
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
}
