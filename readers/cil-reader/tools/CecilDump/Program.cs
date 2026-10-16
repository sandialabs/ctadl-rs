// CecilDump: deterministic text dump of a CIL image on top of Mono.Cecil.
//
// The output format is the contract shared with crates/cil-dump; the root
// `cargo xtask regression --frontend cil` diffs the two, and `cargo xtask cil
// bless` saves this side as the
// goldens in xtask/cil-goldens/.
//
// Determinism notes:
// - No assembly resolution outside [resolve] and [custom-attributes-resolved]:
//   the resolver always fails, so anything else that needs resolution
//   (foreign enum CA arguments) prints `!unresolved`. Those two sections read
//   the image again with a resolver that probes only the input's own
//   directory, as the Rust side's DirectoryLoader does.
// - Floats print as bit patterns (IL float operands also print first as the
//   test Formatter renders them); blobs as lowercase hex; attribute strings
//   escaped, so every record is one line.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;

using Mono.Cecil;
using Mono.Cecil.Cil;
using Mono.Collections.Generic;

static class Program
{
    static readonly string[] AllSections = {
        "module", "assembly", "assembly-refs", "module-refs", "resources",
        "exported-types", "types", "fields", "methods", "properties", "events",
        "custom-attributes", "security-declarations", "il", "resolve", "custom-attributes-resolved",
        "fields-resolved",
    };

    static int Main (string [] args)
    {
        var sections = new List<string> (AllSections);
        string path = null;
        bool symbols = false;

        for (int i = 0; i < args.Length; i++) {
            switch (args [i]) {
            case "--sections":
                sections = args [++i].Split (',').ToList ();
                break;
            case "--no-il":
                sections.Remove ("il");
                break;
            case "--symbols":
                symbols = true;
                break;
            default:
                path = args [i];
                break;
            }
        }

        if (path == null) {
            Console.Error.WriteLine ("usage: CecilDump [--sections a,b,c] [--no-il] [--symbols] <file>");
            return 1;
        }

        if (symbols) {
            Console.Out.Write (DumpSymbols (path));
            return 0;
        }

        var parameters = new ReaderParameters {
            AssemblyResolver = new FailResolver (),
        };

        using var module = ModuleDefinition.ReadModule (path, parameters);
        var o = new StringBuilder ();

        foreach (var section in sections) {
            switch (section) {
            case "module": DumpModule (module, o); break;
            case "assembly": DumpAssembly (module, o); break;
            case "assembly-refs": DumpAssemblyRefs (module, o); break;
            case "module-refs": DumpModuleRefs (module, o); break;
            case "resources": DumpResources (module, o); break;
            case "exported-types": DumpExportedTypes (module, o); break;
            case "types": DumpTypes (module, o); break;
            case "fields": DumpFields (module, o); break;
            case "methods": DumpMethods (module, o); break;
            case "properties": DumpProperties (module, o); break;
            case "events": DumpEvents (module, o); break;
            case "custom-attributes": DumpCustomAttributes (module, o); break;
            case "security-declarations": DumpSecurityDeclarations (module, o); break;
            case "il": DumpIl (module, o); break;
            case "resolve": DumpResolve (path, o); break;
            case "custom-attributes-resolved": DumpCustomAttributesResolved (module, path, o); break;
            case "fields-resolved": DumpFieldsResolved (module, path, o); break;
            default:
                Console.Error.WriteLine ($"unknown section: {section}");
                return 1;
            }
        }

        Console.Out.Write (o.ToString ());
        return 0;
    }

    sealed class FailResolver : IAssemblyResolver
    {
        public AssemblyDefinition Resolve (AssemblyNameReference name)
            => throw new AssemblyResolutionException (name);
        public AssemblyDefinition Resolve (AssemblyNameReference name, ReaderParameters parameters)
            => throw new AssemblyResolutionException (name);
        public void Dispose () { }
    }

    // BaseAssemblyResolver's directory probing over the given directories and
    // nothing else (no framework or GAC fallbacks); a miss is null rather than
    // an exception, as the Rust side's loader returns None. DefaultAssemblyResolver
    // caches by full name.
    sealed class DirectoryResolver : DefaultAssemblyResolver
    {
        readonly string [] directories;

        public DirectoryResolver (params string [] directories)
        {
            this.directories = directories;
        }

        public override AssemblyDefinition Resolve (AssemblyNameReference name, ReaderParameters parameters)
            => SearchDirectory (name, directories, parameters);
    }

    static string Tok (MetadataToken token) => $"0x{token.ToUInt32 ():x8}";

    static string Hex (byte [] bytes)
        => bytes == null || bytes.Length == 0
            ? "-"
            : string.Concat (bytes.Select (b => b.ToString ("x2")));

    static string Inv (IFormattable value) => value.ToString (null, CultureInfo.InvariantCulture);

    static void DumpModule (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[module]\n");
        o.Append ($"name={module.Name}\n");
        o.Append ($"mvid={module.Mvid.ToString ("d")}\n");
        o.Append ($"kind={module.Kind}\n");
        o.Append ($"runtime={module.RuntimeVersion}\n");
        o.Append ($"architecture={module.Architecture}\n");
        o.Append ($"module-attributes=0x{(uint) module.Attributes:x8}\n");
        var ep = module.EntryPoint;
        o.Append (ep != null ? $"entry-point={Tok (ep.MetadataToken)}\n" : "entry-point=-\n");
    }

    static void DumpAssembly (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[assembly]\n");
        if (module.Assembly == null)
            return;
        var name = module.Assembly.Name;
        o.Append ($"full-name={name.FullName}\n");
        o.Append ($"hash-algorithm=0x{(uint) name.HashAlgorithm:x8}\n");
        o.Append ($"flags=0x{(uint) name.Attributes:x8}\n");
        o.Append ($"public-key={Hex (name.PublicKey)}\n");
    }

    static void DumpAssemblyRefs (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[assembly-refs]\n");
        foreach (var r in module.AssemblyReferences)
            o.Append ($"{Tok (r.MetadataToken)}={r.FullName} flags=0x{(uint) r.Attributes:x8}\n");
    }

    static void DumpModuleRefs (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[module-refs]\n");
        foreach (var r in module.ModuleReferences)
            o.Append ($"{Tok (r.MetadataToken)}={r.Name}\n");
    }

    static void DumpResources (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[resources]\n");
        foreach (var r in module.Resources) {
            switch (r) {
            case EmbeddedResource embedded: {
                long size;
                try {
                    size = embedded.GetResourceData ().Length;
                } catch {
                    size = -1;
                }
                o.Append ($"embedded name={r.Name} flags=0x{(uint) embedded.Attributes:x8} size={size}\n");
                break;
            }
            case LinkedResource linked:
                o.Append ($"linked name={r.Name} flags=0x{(uint) linked.Attributes:x8} file={linked.File}\n");
                break;
            case AssemblyLinkedResource al:
                o.Append ($"assembly-linked name={r.Name} flags=0x{(uint) al.Attributes:x8} assembly={al.Assembly.FullName}\n");
                break;
            }
        }
    }

    static void DumpExportedTypes (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[exported-types]\n");
        foreach (var et in module.ExportedTypes) {
            string scope;
            try {
                scope = et.Scope?.Name ?? ScopeThroughDeclaring (et);
            } catch {
                scope = "!error";
            }
            o.Append ($"{Tok (et.MetadataToken)}={et.FullName} scope={scope} flags=0x{(uint) et.Attributes:x8}\n");
        }
    }

    static string ScopeThroughDeclaring (ExportedType et)
    {
        while (et.DeclaringType != null)
            et = et.DeclaringType;
        return et.Scope?.Name ?? "!error";
    }

    static IEnumerable<TypeDefinition> TypesInRidOrder (ModuleDefinition module)
    {
        for (uint rid = 1; ; rid++) {
            TypeDefinition type;
            try {
                type = module.LookupToken (new MetadataToken (TokenType.TypeDef, rid)) as TypeDefinition;
            } catch {
                yield break;
            }
            if (type == null)
                yield break;
            yield return type;
        }
    }

    // Fields and methods are reached through their declaring types, as
    // properties and events are, and collected in rid order. Probing
    // LookupToken by rid would not do: on a module whose types are not yet
    // read, Cecil finds a member's owner by searching the TypeDef list ranges
    // with the member's rid, which is wrong when FieldPtr/MethodPtr are
    // present, and returns null for members it then fails to find.
    static IEnumerable<T> MembersInRidOrder<T> (ModuleDefinition module, Func<TypeDefinition, IEnumerable<T>> members)
        where T : IMetadataTokenProvider
    {
        var byRid = new SortedDictionary<uint, T> ();
        try {
            foreach (var ty in TypesInRidOrder (module))
                foreach (var member in members (ty))
                    byRid [member.MetadataToken.RID] = member;
        } catch {
        }
        return byRid.Values;
    }

    static void DumpTypes (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[types]\n");
        foreach (var ty in TypesInRidOrder (module)) {
            o.Append ($"{Tok (ty.MetadataToken)}={ty.FullName} flags=0x{(uint) ty.Attributes:x8}");
            if (ty.IsValueType)
                o.Append (" valuetype");
            if (ty.IsEnum)
                o.Append (" enum");
            o.Append ('\n');
            if (ty.BaseType != null)
                o.Append ($"\tbase={ty.BaseType.FullName}\n");
            if (ty.HasLayoutInfo)
                o.Append ($"\tlayout packing={ty.PackingSize} size={ty.ClassSize}\n");
            foreach (var iface in ty.Interfaces)
                o.Append ($"\tinterface={iface.InterfaceType.FullName}\n");
            foreach (var gp in ty.GenericParameters) {
                o.Append ($"\tgeneric-param {gp.Position} name={gp.Name} flags=0x{(ushort) gp.Attributes:x4}\n");
                foreach (var c in gp.Constraints)
                    o.Append ($"\t\tconstraint={c.ConstraintType.FullName}\n");
            }
        }
    }

    static void DumpFields (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[fields]\n");
        foreach (var field in MembersInRidOrder (module, ty => ty.Fields)) {
            string fullName;
            try {
                fullName = field.FullName;
            } catch {
                fullName = "!error";
            }
            o.Append ($"{Tok (field.MetadataToken)}={fullName} flags=0x{(ushort) field.Attributes:x4}");
            if (field.HasLayoutInfo)
                o.Append ($" offset={field.Offset}");
            o.Append (RvaPart (field));
            if (field.HasConstant)
                o.Append ($" constant={FormatConstant (field.Constant)}");
            if (field.HasMarshalInfo)
                o.Append ($" marshal={FormatMarshal (field.MarshalInfo)}");
            o.Append ('\n');
        }
    }

    static string FormatConstant (object value)
    {
        if (value == null)
            return "Null";
        switch (value) {
        case bool b: return $"Boolean:{(b ? "True" : "False")}";
        case char c: return $"Char:{(ushort) c}";
        case sbyte v: return $"I1:{Inv (v)}";
        case byte v: return $"U1:{Inv (v)}";
        case short v: return $"I2:{Inv (v)}";
        case ushort v: return $"U2:{Inv (v)}";
        case int v: return $"I4:{Inv (v)}";
        case uint v: return $"U4:{Inv (v)}";
        case long v: return $"I8:{Inv (v)}";
        case ulong v: return $"U8:{Inv (v)}";
        case float v: return $"R4:0x{(uint) BitConverter.SingleToInt32Bits (v):x8}";
        case double v: return $"R8:0x{(ulong) BitConverter.DoubleToInt64Bits (v):x16}";
        case string s: return $"String:\"{s}\"";
        default: return $"?{value.GetType ().Name}";
        }
    }

    static string FormatMarshal (MarshalInfo info)
    {
        switch (info) {
        case ArrayMarshalInfo array:
            return $"array(elem=0x{(byte) array.ElementType:x2},size={array.Size},index={array.SizeParameterIndex},mult={array.SizeParameterMultiplier})";
        case SafeArrayMarshalInfo safe:
            return $"safearray(0x{(byte) safe.ElementType:x2})";
        case FixedArrayMarshalInfo fixedArray:
            return $"fixedarray(size={fixedArray.Size},elem=0x{(byte) fixedArray.ElementType:x2})";
        case FixedSysStringMarshalInfo str:
            return $"fixedsysstring({str.Size})";
        case CustomMarshalInfo custom:
            return $"custom(guid={custom.Guid.ToString ("d")},unmanaged={custom.UnmanagedType},managed={custom.ManagedType?.FullName},cookie={custom.Cookie})";
        default:
            return $"native(0x{(byte) info.NativeType:x2})";
        }
    }

    static void DumpMethods (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[methods]\n");
        foreach (var method in MembersInRidOrder (module, ty => ty.Methods)) {
            string fullName;
            try {
                fullName = method.FullName;
            } catch {
                fullName = "!error";
            }
            o.Append ($"{Tok (method.MetadataToken)}={fullName} flags=0x{(ushort) method.Attributes:x4} impl=0x{(ushort) method.ImplAttributes:x4} rva=0x{(uint) method.RVA:x8}");
            var semantics = (ushort) method.SemanticsAttributes;
            if (semantics != 0)
                o.Append ($" semantics=0x{semantics:x4}");
            if (method.HasPInvokeInfo && method.PInvokeInfo != null)
                o.Append ($" pinvoke=({method.PInvokeInfo.Module.Name},{method.PInvokeInfo.EntryPoint},0x{(ushort) method.PInvokeInfo.Attributes:x4})");
            o.Append ('\n');
            foreach (var gp in method.GenericParameters)
                o.Append ($"\tgeneric-param {gp.Position} name={gp.Name} flags=0x{(ushort) gp.Attributes:x4}\n");
            foreach (var over in method.Overrides)
                o.Append ($"\toverride={Safe (() => over.FullName)}\n");
            foreach (var p in method.Parameters) {
                if (p.MetadataToken.RID == 0)
                    continue;
                o.Append ($"\tparam {p.Sequence} name={p.Name} flags=0x{(ushort) p.Attributes:x4}");
                if (p.HasConstant)
                    o.Append ($" constant={FormatConstant (p.Constant)}");
                if (p.HasMarshalInfo)
                    o.Append ($" marshal={FormatMarshal (p.MarshalInfo)}");
                o.Append ('\n');
            }
        }
    }

    // Every TypeRef, TypeSpec and MemberRef row, and the definition it
    // resolves to: `-` for null, `!error` when resolution throws.
    static void DumpResolve (string path, StringBuilder o)
    {
        o.Append ("[resolve]\n");
        var directory = System.IO.Path.GetDirectoryName (System.IO.Path.GetFullPath (path));
        var parameters = new ReaderParameters {
            AssemblyResolver = new DirectoryResolver (directory),
        };
        using var module = ModuleDefinition.ReadModule (path, parameters);
        var resolver = module.MetadataResolver;

        var type_refs = module.GetTypeReferences ().ToArray ();
        for (int i = 0; i < type_refs.Length; i++)
            ResolveLine (new MetadataToken (TokenType.TypeRef, i + 1), () => resolver.Resolve (type_refs [i]), o);

        var type_specs = module.Image.GetTableLength (Mono.Cecil.Metadata.Table.TypeSpec);
        for (int rid = 1; rid <= type_specs; rid++) {
            var token = new MetadataToken (TokenType.TypeSpec, rid);
            ResolveLine (token, () => resolver.Resolve ((TypeReference) module.LookupToken (token)), o);
        }

        var member_refs = module.GetMemberReferences ().ToArray ();
        for (int i = 0; i < member_refs.Length; i++) {
            var token = new MetadataToken (TokenType.MemberRef, i + 1);
            ResolveLine (token, () => member_refs [i] switch {
                FieldReference field => resolver.Resolve (field),
                MethodReference method => resolver.Resolve (method),
                _ => null,
            }, o);
        }
    }

    static void ResolveLine (MetadataToken token, Func<IMemberDefinition> resolve, StringBuilder o)
    {
        string result;
        try {
            var definition = resolve ();
            result = definition == null
                ? "-"
                : $"{((MemberReference) definition).Module.Name}!{Tok (definition.MetadataToken)} {definition.FullName}";
        } catch {
            result = "!error";
        }
        o.Append ($"{Tok (token)} -> {result}\n");
    }

    static string Safe (Func<string> f)
    {
        try {
            return f ();
        } catch {
            return "!error";
        }
    }

    static void DumpProperties (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[properties]\n");
        // Property rows are reached through their declaring types; collect in
        // rid order.
        var properties = new SortedDictionary<uint, PropertyDefinition> ();
        foreach (var ty in TypesInRidOrder (module))
            foreach (var p in ty.Properties)
                properties [p.MetadataToken.RID] = p;
        foreach (var p in properties.Values) {
            o.Append ($"{Tok (p.MetadataToken)}={Safe (() => p.FullName)} flags=0x{(ushort) p.Attributes:x4}");
            if (p.GetMethod != null)
                o.Append ($" getter={Tok (p.GetMethod.MetadataToken)}");
            if (p.SetMethod != null)
                o.Append ($" setter={Tok (p.SetMethod.MetadataToken)}");
            if (p.HasOtherMethods)
                o.Append ($" other={OtherMethods (p.OtherMethods)}");
            if (p.HasConstant)
                o.Append ($" constant={FormatConstant (p.Constant)}");
            o.Append ('\n');
        }
    }

    static string OtherMethods (IEnumerable<MethodDefinition> methods)
        => string.Join (",", methods.Select (m => Tok (m.MetadataToken)));

    static void DumpEvents (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[events]\n");
        var events = new SortedDictionary<uint, EventDefinition> ();
        foreach (var ty in TypesInRidOrder (module))
            foreach (var e in ty.Events)
                events [e.MetadataToken.RID] = e;
        foreach (var e in events.Values) {
            o.Append ($"{Tok (e.MetadataToken)}={e.Name} flags=0x{(ushort) e.Attributes:x4}");
            if (e.EventType != null)
                o.Append ($" type={Safe (() => e.EventType.FullName)}");
            if (e.AddMethod != null)
                o.Append ($" add={Tok (e.AddMethod.MetadataToken)}");
            if (e.RemoveMethod != null)
                o.Append ($" remove={Tok (e.RemoveMethod.MetadataToken)}");
            if (e.InvokeMethod != null)
                o.Append ($" fire={Tok (e.InvokeMethod.MetadataToken)}");
            if (e.HasOtherMethods)
                o.Append ($" other={OtherMethods (e.OtherMethods)}");
            o.Append ('\n');
        }
    }

    static void DumpCustomAttributes (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[custom-attributes]\n");
        foreach (var (parent, attr) in SortedAttributes (module))
            o.Append (AttributeLine (parent, attr)).Append ('\n');
    }

    // The custom attributes whose line changes when their enum arguments'
    // assemblies resolve from the input's directory, as [resolve]'s do, with
    // that line. `module` is the image read without resolution; attributes
    // read their arguments once, so the resolved ones come from a second read.
    static void DumpCustomAttributesResolved (ModuleDefinition module, string path, StringBuilder o)
    {
        o.Append ("[custom-attributes-resolved]\n");
        var directory = System.IO.Path.GetDirectoryName (System.IO.Path.GetFullPath (path));
        var parameters = new ReaderParameters {
            AssemblyResolver = new DirectoryResolver (directory),
        };
        using var resolving = ModuleDefinition.ReadModule (path, parameters);
        ReadAsDumped (module);
        ReadAsDumped (resolving);
        var unresolved = SortedAttributes (module);
        var resolved = SortedAttributes (resolving);
        for (int i = 0; i < resolved.Count; i++) {
            var line = AttributeLine (resolved [i].parent, resolved [i].attr);
            if (line != AttributeLine (unresolved [i].parent, unresolved [i].attr))
                o.Append (line).Append ('\n');
        }
    }

    // A field's ` rva=... rva-data=...`: reading the RVA sizes the field's
    // type, which resolves it with the module's assembly resolver.
    static string RvaPart (FieldDefinition field)
    {
        try {
            return field.RVA != 0
                ? $" rva=0x{(uint) field.RVA:x8} rva-data={Hex (field.InitialValue)}"
                : "";
        } catch {
            return " rva-data=!error";
        }
    }

    // The fields whose RVA changes when their types' assemblies resolve from
    // the input's directory, as [resolve]'s do, with that RVA. `module` is
    // the image read without resolution.
    static void DumpFieldsResolved (ModuleDefinition module, string path, StringBuilder o)
    {
        o.Append ("[fields-resolved]\n");
        var directory = System.IO.Path.GetDirectoryName (System.IO.Path.GetFullPath (path));
        var parameters = new ReaderParameters {
            AssemblyResolver = new DirectoryResolver (directory),
        };
        using var resolving = ModuleDefinition.ReadModule (path, parameters);
        ReadAsDumped (module);
        ReadAsDumped (resolving);
        var unresolved = MembersInRidOrder (module, ty => ty.Fields).ToDictionary (f => f.MetadataToken.RID);
        foreach (var field in MembersInRidOrder (resolving, ty => ty.Fields)) {
            var rva = RvaPart (field);
            if (!unresolved.TryGetValue (field.MetadataToken.RID, out var other) || rva != RvaPart (other))
                o.Append ($"{Tok (field.MetadataToken)}{rva}\n");
        }
    }

    // Reads `module` as the sections before [custom-attributes] do. What
    // Cecil reads depends on the order: in an image with Ptr tables, a
    // MethodDef attribute constructor looked up before its type's members
    // are read comes back null.
    static void ReadAsDumped (ModuleDefinition module)
    {
        var scratch = new StringBuilder ();
        DumpModule (module, scratch);
        DumpAssembly (module, scratch);
        DumpAssemblyRefs (module, scratch);
        DumpModuleRefs (module, scratch);
        DumpResources (module, scratch);
        DumpExportedTypes (module, scratch);
        DumpTypes (module, scratch);
        DumpFields (module, scratch);
        DumpMethods (module, scratch);
        DumpProperties (module, scratch);
        DumpEvents (module, scratch);
    }

    static List<(MetadataToken parent, CustomAttribute attr)> SortedAttributes (ModuleDefinition module)
    {
        // Walk the raw table via GetCustomAttributes? Cecil exposes attributes
        // per provider; recover rid order through the read-order token.
        var attributes = new List<(MetadataToken parent, CustomAttribute attr)> ();
        CollectAttributes (module, attributes);
        // The CustomAttribute table is sorted by its HasCustomAttribute coded
        // parent index, so sorting collected attributes by that key (stably)
        // recovers rid order.
        return attributes
            .Select ((entry, index) => (entry, index))
            .OrderBy (x => CodedParentKey (x.entry.parent))
            .ThenBy (x => x.index)
            .Select (x => x.entry)
            .ToList ();
    }

    static string AttributeLine (MetadataToken parent, CustomAttribute attr)
    {
        var line = $"ca parent={Tok (parent)} ctor={Safe (() => attr.Constructor.FullName)}";
        string args;
        try {
            args = FormatAttributeArgs (attr);
        } catch {
            args = null;
        }
        return args != null
            ? $"{line} args={args}"
            : $"{line} args=!unresolved blob={Hex (attr.GetBlob ())}";
    }

    static ulong CodedParentKey (MetadataToken token)
    {
        // HasCustomAttribute tag order (Utilities.cs): Method, Field, TypeRef,
        // TypeDef, Param, InterfaceImpl, MemberRef, Module, Permission,
        // Property, Event, Signature, ModuleRef, TypeSpec, Assembly,
        // AssemblyRef, File, ExportedType, ManifestResource, GenericParam,
        // GenericParamConstraint, MethodSpec.
        uint tag = token.TokenType switch {
            TokenType.Method => 0,
            TokenType.Field => 1,
            TokenType.TypeRef => 2,
            TokenType.TypeDef => 3,
            TokenType.Param => 4,
            TokenType.InterfaceImpl => 5,
            TokenType.MemberRef => 6,
            TokenType.Module => 7,
            TokenType.Permission => 8,
            TokenType.Property => 9,
            TokenType.Event => 10,
            TokenType.Signature => 11,
            TokenType.ModuleRef => 12,
            TokenType.TypeSpec => 13,
            TokenType.Assembly => 14,
            TokenType.AssemblyRef => 15,
            TokenType.File => 16,
            TokenType.ExportedType => 17,
            TokenType.ManifestResource => 18,
            TokenType.GenericParam => 19,
            TokenType.GenericParamConstraint => 20,
            TokenType.MethodSpec => 21,
            _ => 31,
        };
        return ((ulong) token.RID << 5) | tag;
    }

    static void CollectAttributes (ModuleDefinition module,
        List<(MetadataToken, CustomAttribute)> sink)
    {
        void Add (ICustomAttributeProvider provider, MetadataToken token)
        {
            if (provider == null || !provider.HasCustomAttributes)
                return;
            foreach (var attr in provider.CustomAttributes)
                sink.Add ((token, attr));
        }

        Add (module, module.MetadataToken);
        if (module.Assembly != null)
            Add (module.Assembly, module.Assembly.MetadataToken);
        foreach (var ty in TypesInRidOrder (module)) {
            Add (ty, ty.MetadataToken);
            foreach (var gp in ty.GenericParameters) {
                Add (gp, gp.MetadataToken);
                foreach (var c in gp.Constraints)
                    Add (c, c.MetadataToken);
            }
            foreach (var iface in ty.Interfaces)
                Add (iface, iface.MetadataToken);
            foreach (var f in ty.Fields)
                Add (f, f.MetadataToken);
            foreach (var m in ty.Methods) {
                Add (m, m.MetadataToken);
                Add (m.MethodReturnType, m.MethodReturnType.MetadataToken);
                foreach (var gp in m.GenericParameters) {
                    Add (gp, gp.MetadataToken);
                    foreach (var c in gp.Constraints)
                        Add (c, c.MetadataToken);
                }
                foreach (var p in m.Parameters)
                    Add (p, p.MetadataToken);
            }
            foreach (var p in ty.Properties)
                Add (p, p.MetadataToken);
            foreach (var e in ty.Events)
                Add (e, e.MetadataToken);
        }
    }

    static string FormatAttributeArgs (CustomAttribute attr)
    {
        var s = new StringBuilder ();
        s.Append ('(');
        for (int i = 0; i < attr.ConstructorArguments.Count; i++) {
            if (i > 0)
                s.Append (", ");
            PpArgument (attr.ConstructorArguments [i], s);
        }
        s.Append (')');
        AppendNamedArgs (attr.Fields, attr.Properties, s);
        return s.ToString ();
    }

    // The declarations in DeclSecurity rid order, which sorting by the
    // HasDeclSecurity coded parent recovers: parent, action, then each
    // security attribute. Resolving a declaration does not swallow an enum
    // that fails to resolve, as attributes do, so any exception prints the
    // blob.
    static void DumpSecurityDeclarations (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[security-declarations]\n");
        var declarations = new List<(MetadataToken parent, SecurityDeclaration decl)> ();
        void Add (ISecurityDeclarationProvider provider)
        {
            if (provider == null || !provider.HasSecurityDeclarations)
                return;
            foreach (var decl in provider.SecurityDeclarations)
                declarations.Add ((provider.MetadataToken, decl));
        }

        foreach (var ty in TypesInRidOrder (module)) {
            Add (ty);
            foreach (var m in ty.Methods)
                Add (m);
        }
        Add (module.Assembly);

        // HasDeclSecurity tag order: TypeDef, MethodDef, Assembly.
        static ulong Key (MetadataToken token)
        {
            uint tag = token.TokenType switch {
                TokenType.TypeDef => 0,
                TokenType.Method => 1,
                _ => 2,
            };
            return ((ulong) token.RID << 2) | tag;
        }

        var sorted = declarations
            .Select ((entry, index) => (entry, index))
            .OrderBy (x => Key (x.entry.parent))
            .ThenBy (x => x.index)
            .Select (x => x.entry);
        foreach (var (parent, decl) in sorted) {
            o.Append ($"sec parent={Tok (parent)} action={(ushort) decl.Action}");
            string attributes;
            try {
                var s = new StringBuilder ();
                foreach (var a in decl.SecurityAttributes) {
                    s.Append ($" attr={a.AttributeType.FullName}");
                    AppendNamedArgs (a.Fields, a.Properties, s);
                }
                attributes = s.ToString ();
            } catch {
                attributes = $" !unresolved blob={Hex (decl.GetBlob ())}";
            }
            o.Append (attributes).Append ('\n');
        }
    }

    static void AppendNamedArgs (IEnumerable<CustomAttributeNamedArgument> fields,
        IEnumerable<CustomAttributeNamedArgument> properties, StringBuilder s)
    {
        foreach (var f in fields) {
            s.Append ($" field {f.Name}=");
            PpArgument (f.Argument, s);
        }
        foreach (var p in properties) {
            s.Append ($" property {p.Name}=");
            PpArgument (p.Argument, s);
        }
    }

    static void PpArgument (CustomAttributeArgument argument, StringBuilder s)
    {
        s.Append ('(');
        PpType (argument.Type, s);
        s.Append (':');
        PpValue (argument.Value, s);
        s.Append (')');
    }

    static void PpType (TypeReference type, StringBuilder s)
    {
        if (type.IsArray) {
            PpType (((ArrayType) type).ElementType, s);
            s.Append ("[]");
        } else if (type.FullName == "System.Type") {
            s.Append ("Type");
        } else {
            switch (type.MetadataType) {
            case MetadataType.Boolean: s.Append ("Boolean"); break;
            case MetadataType.Char: s.Append ("Char"); break;
            case MetadataType.SByte: s.Append ("I1"); break;
            case MetadataType.Byte: s.Append ("U1"); break;
            case MetadataType.Int16: s.Append ("I2"); break;
            case MetadataType.UInt16: s.Append ("U2"); break;
            case MetadataType.Int32: s.Append ("I4"); break;
            case MetadataType.UInt32: s.Append ("U4"); break;
            case MetadataType.Int64: s.Append ("I8"); break;
            case MetadataType.UInt64: s.Append ("U8"); break;
            case MetadataType.Single: s.Append ("R4"); break;
            case MetadataType.Double: s.Append ("R8"); break;
            case MetadataType.String: s.Append ("String"); break;
            case MetadataType.Object: s.Append ("Object"); break;
            default: s.Append (type.FullName); break;
            }
        }
    }

    static void PpValue (object value, StringBuilder s)
    {
        if (value == null) {
            s.Append ("null");
            return;
        }
        if (value is CustomAttributeArgument [] arguments) {
            s.Append ('{');
            for (int i = 0; i < arguments.Length; i++) {
                if (i > 0)
                    s.Append (", ");
                PpArgument (arguments [i], s);
            }
            s.Append ('}');
            return;
        }
        if (value is CustomAttributeArgument boxed) {
            PpArgument (boxed, s);
            return;
        }
        switch (value) {
        case string str: AppendQuoted (str, s); break;
        case char c: s.Append ($"char:0x{(ushort) c:x4}"); break;
        case bool b: s.Append (b ? "True" : "False"); break;
        case float f: s.Append ($"r4:0x{(uint) BitConverter.SingleToInt32Bits (f):x8}"); break;
        case double d: s.Append ($"r8:0x{(ulong) BitConverter.DoubleToInt64Bits (d):x16}"); break;
        case TypeReference tr: s.Append ($"typeof({tr.FullName})"); break;
        case IFormattable formattable: s.Append (Inv (formattable)); break;
        default: s.Append (value.ToString ()); break;
        }
    }

    // `value` in double quotes, with `\` and control characters escaped as
    // cil-dump escapes them.
    static void AppendQuoted (string value, StringBuilder s)
    {
        s.Append ('"');
        foreach (var c in value) {
            switch (c) {
            case '\\': s.Append ("\\\\"); break;
            case '\r': s.Append ("\\r"); break;
            case '\n': s.Append ("\\n"); break;
            case '\t': s.Append ("\\t"); break;
            default:
                if (c < ' ' || c == '\x7f')
                    s.Append ($"\\x{(int) c:x2}");
                else
                    s.Append (c);
                break;
            }
        }
        s.Append ('"');
    }

    // The sequence points of the module's symbols, as Cecil reads them with
    // ReadSymbols: `none` without symbols (native PDBs and .mdb files are not
    // compiled in, so read as none), `!error` where reading them throws. The
    // documents the points name, by token, then every method body's points
    // as raw numbers. The format is crates/cil-dump's `dump_symbols`.
    static string DumpSymbols (string path)
    {
        var o = new StringBuilder ("[symbols]\n");
        ModuleDefinition module;
        try {
            module = ModuleDefinition.ReadModule (path, new ReaderParameters {
                AssemblyResolver = new FailResolver (),
                ReadSymbols = true,
                SymbolReaderProvider = new DefaultSymbolReaderProvider (throwIfNoSymbol: false),
            });
        } catch {
            o.Append ("!error\n");
            return o.ToString ();
        }
        using (module) {
            if (!module.HasSymbols) {
                o.Append ("none\n");
                return o.ToString ();
            }
            var methods = new StringBuilder ();
            var documents = new SortedDictionary<uint, Document> ();
            foreach (var method in MembersInRidOrder (module, ty => ty.Methods)) {
                if (!method.HasBody || method.RVA == 0)
                    continue;
                Collection<SequencePoint> points;
                try {
                    _ = method.Body;
                    points = method.DebugInformation.SequencePoints;
                } catch {
                    methods.Append ($"{Tok (method.MetadataToken)} !error\n");
                    continue;
                }
                if (points.Count == 0)
                    continue;
                methods.Append ($"{Tok (method.MetadataToken)}\n");
                foreach (var p in points) {
                    documents [p.Document.MetadataToken.RID] = p.Document;
                    methods.Append ($"\tIL_{p.Offset:x4} {Tok (p.Document.MetadataToken)} {p.StartLine},{p.StartColumn}-{p.EndLine},{p.EndColumn}\n");
                }
            }
            foreach (var d in documents.Values)
                o.Append ($"document {Tok (d.MetadataToken)} name={d.Url} hash-algorithm={d.HashAlgorithmGuid.ToString ("d")} hash={Hex (d.Hash)} language={d.LanguageGuid.ToString ("d")}\n");
            o.Append (methods);
        }
        return o.ToString ();
    }

    static void DumpIl (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[il]\n");
        foreach (var method in MembersInRidOrder (module, ty => ty.Methods)) {
            if (!method.HasBody || method.RVA == 0)
                continue;

            MethodBody body;
            try {
                body = method.Body;
            } catch {
                o.Append ($"{Tok (method.MetadataToken)} !error\n");
                continue;
            }

            o.Append ($"{Tok (method.MetadataToken)} max-stack={body.MaxStackSize} init-locals={(body.InitLocals ? "true" : "false")} code-size={body.CodeSize}\n");
            for (int i = 0; i < body.Variables.Count; i++)
                o.Append ($"\tlocal {i}={Safe (() => body.Variables [i].VariableType.FullName)}\n");

            var lastOffset = body.Instructions.Count > 0 ? body.Instructions [body.Instructions.Count - 1].Offset : -1;
            foreach (var ins in body.Instructions) {
                o.Append ('\t');
                o.Append (Label (ins.Offset));
                o.Append (": ");
                o.Append (ins.OpCode.Name);
                if (ins.Operand != null) {
                    o.Append (' ');
                    WriteOperand (ins.Operand, o);
                }
                o.Append ('\n');
            }

            foreach (var h in body.ExceptionHandlers) {
                string kind;
                switch (h.HandlerType) {
                case ExceptionHandlerType.Catch:
                    kind = $"catch {(h.CatchType != null ? Safe (() => h.CatchType.FullName) : "-")}";
                    break;
                case ExceptionHandlerType.Filter:
                    kind = $"filter {LabelOrEnd (h.FilterStart, body)}";
                    break;
                case ExceptionHandlerType.Finally:
                    kind = "finally";
                    break;
                default:
                    kind = "fault";
                    break;
                }
                o.Append ($"\t.try {LabelOrEnd (h.TryStart, body)} to {LabelOrEnd (h.TryEnd, body)} {kind} handler {LabelOrEnd (h.HandlerStart, body)} to {LabelOrEnd (h.HandlerEnd, body)}\n");
            }
        }
    }

    static string Label (int offset) => $"IL_{offset:x4}";

    static string LabelOrEnd (Instruction instruction, MethodBody body)
        => instruction != null ? Label (instruction.Offset) : "IL_END";

    static void WriteOperand (object operand, StringBuilder o)
    {
        switch (operand) {
        case Instruction target:
            o.Append (Label (target.Offset));
            break;
        case Instruction [] targets: {
            o.Append ('(');
            for (int i = 0; i < targets.Length; i++) {
                if (i > 0)
                    o.Append (", ");
                o.Append (targets [i] != null ? Label (targets [i].Offset) : "IL_END");
            }
            o.Append (')');
            break;
        }
        case string s:
            o.Append ($"\"{s}\"");
            break;
        case ParameterDefinition parameter:
            o.Append (Inv (parameter.Sequence));
            break;
        case VariableDefinition variable:
            o.Append ($"V_{variable.Index}");
            break;
        // The test Formatter's rendering, then the exact bits.
        case float f:
            o.Append ($"{Inv (f)} r4:0x{(uint) BitConverter.SingleToInt32Bits (f):x8}");
            break;
        case double d:
            o.Append ($"{Inv (d)} r8:0x{(ulong) BitConverter.DoubleToInt64Bits (d):x16}");
            break;
        case sbyte sb:
            o.Append (Inv (sb));
            break;
        case byte b:
            o.Append (Inv (b));
            break;
        case CallSite callSite:
            o.Append (Safe (() => callSite.FullName));
            break;
        case MemberReference member:
            o.Append (Safe (() => member.FullName));
            break;
        case IFormattable formattable:
            o.Append (Inv (formattable));
            break;
        default:
            o.Append (operand.ToString ());
            break;
        }
    }
}
