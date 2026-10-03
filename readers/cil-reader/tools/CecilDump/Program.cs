// CecilDump: deterministic text dump of a CIL image on top of Mono.Cecil.
//
// The output format is the contract shared with crates/cil-dump; the root
// `cargo xtask regression --frontend cil` diffs the two, and `cargo xtask cil
// bless` saves this side as the
// goldens in xtask/cil-goldens/.
//
// Determinism notes:
// - No assembly resolution: the resolver always fails, so anything that needs
//   resolution (foreign enum CA arguments) prints `!unresolved` exactly like
//   the Rust side, which has no resolver until M7.
// - Floats print as bit patterns; blobs as lowercase hex.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;

using Mono.Cecil;
using Mono.Cecil.Cil;

static class Program
{
    static readonly string[] AllSections = {
        "module", "assembly", "assembly-refs", "module-refs", "resources",
        "exported-types", "types", "fields", "methods", "properties", "events",
        "custom-attributes", "il",
    };

    static int Main (string [] args)
    {
        var sections = new List<string> (AllSections);
        string path = null;

        for (int i = 0; i < args.Length; i++) {
            switch (args [i]) {
            case "--sections":
                sections = args [++i].Split (',').ToList ();
                break;
            case "--no-il":
                sections.Remove ("il");
                break;
            default:
                path = args [i];
                break;
            }
        }

        if (path == null) {
            Console.Error.WriteLine ("usage: CecilDump [--sections a,b,c] [--no-il] <file>");
            return 1;
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
            case "il": DumpIl (module, o); break;
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
            o.Append ($"{Tok (r.MetadataToken)}={r.FullName}\n");
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
            o.Append ($"{Tok (ty.MetadataToken)}={ty.FullName} flags=0x{(uint) ty.Attributes:x8}\n");
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
            try {
                if (field.RVA != 0)
                    o.Append ($" rva-data={Hex (field.InitialValue)}");
            } catch {
                o.Append (" rva-data=!error");
            }
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
            if (p.HasConstant)
                o.Append ($" constant={FormatConstant (p.Constant)}");
            o.Append ('\n');
        }
    }

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
            o.Append ('\n');
        }
    }

    static void DumpCustomAttributes (ModuleDefinition module, StringBuilder o)
    {
        o.Append ("[custom-attributes]\n");
        // Walk the raw table via GetCustomAttributes? Cecil exposes attributes
        // per provider; recover rid order through the read-order token.
        var attributes = new List<(MetadataToken parent, CustomAttribute attr)> ();
        CollectAttributes (module, attributes);
        // The CustomAttribute table is sorted by its HasCustomAttribute coded
        // parent index, so sorting collected attributes by that key (stably)
        // recovers rid order.
        var sorted = attributes
            .Select ((entry, index) => (entry, index))
            .OrderBy (x => CodedParentKey (x.entry.parent))
            .ThenBy (x => x.index)
            .Select (x => x.entry)
            .ToList ();
        foreach (var (parent, attr) in sorted) {
            o.Append ($"ca parent={Tok (parent)} ctor={Safe (() => attr.Constructor.FullName)}");
            string args;
            try {
                args = FormatAttributeArgs (attr);
            } catch {
                args = null;
            }
            if (args != null)
                o.Append ($" args={args}");
            else
                o.Append ($" args=!unresolved blob={Hex (attr.GetBlob ())}");
            o.Append ('\n');
        }
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
        foreach (var f in attr.Fields) {
            s.Append ($" field {f.Name}=");
            PpArgument (f.Argument, s);
        }
        foreach (var p in attr.Properties) {
            s.Append ($" property {p.Name}=");
            PpArgument (p.Argument, s);
        }
        return s.ToString ();
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
        case string str: s.Append ($"\"{str}\""); break;
        case char c: s.Append ($"char:0x{(ushort) c:x4}"); break;
        case bool b: s.Append (b ? "True" : "False"); break;
        case float f: s.Append ($"r4:0x{(uint) BitConverter.SingleToInt32Bits (f):x8}"); break;
        case double d: s.Append ($"r8:0x{(ulong) BitConverter.DoubleToInt64Bits (d):x16}"); break;
        case TypeReference tr: s.Append ($"typeof({tr.FullName})"); break;
        case IFormattable formattable: s.Append (Inv (formattable)); break;
        default: s.Append (value.ToString ()); break;
        }
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
        case float f:
            o.Append ($"r4:0x{(uint) BitConverter.SingleToInt32Bits (f):x8}");
            break;
        case double d:
            o.Append ($"r8:0x{(ulong) BitConverter.DoubleToInt64Bits (d):x16}");
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
