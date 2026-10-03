// PtrGen <in.dll> <out.dll>: rewrites a module so that its member tables go
// through FieldPtr, MethodPtr, ParamPtr, PropertyPtr and EventPtr. Roslyn and
// ilasm never emit those tables, but dnlib does when it saves with
// PreserveRids and members are out of declaring-type order (as de4dot
// --preserve-tokens and dnlib-based obfuscators do).
//
// New rows take the next free rids but are inserted first in early owners, so
// each Ptr table is a non-identity permutation:
// - a field, a method, a property and an event, at the front of the first
//   non-<Module> type;
// - a return-parameter row (sequence 0) on the lowest-rid method lacking one.
//   dnlib orders params by the owning method's rid, so a param of the new
//   method would sort last and need no ParamPtr.
using System;
using System.Linq;
using dnlib.DotNet;
using dnlib.DotNet.Emit;
using dnlib.DotNet.Writer;

static class Program {
    const MethodImplAttributes Impl = MethodImplAttributes.IL | MethodImplAttributes.Managed;
    const MethodAttributes Instance = MethodAttributes.Public | MethodAttributes.HideBySig;
    const MethodAttributes Accessor = Instance | MethodAttributes.SpecialName;

    static MethodDef Method(string name, MethodSig sig, MethodAttributes attrs, params Instruction[] body) {
        var m = new MethodDefUser(name, sig, Impl, attrs) { Body = new CilBody() };
        foreach (var i in body)
            m.Body.Instructions.Add(i);
        m.Body.Instructions.Add(OpCodes.Ret.ToInstruction());
        return m;
    }

    static int Main(string[] args) {
        if (args.Length != 2) {
            Console.Error.WriteLine("usage: PtrGen <in.dll> <out.dll>");
            return 2;
        }
        var mod = ModuleDefMD.Load(args[0]);
        var cs = mod.CorLibTypes;
        var t = mod.Types.First(x => !x.IsGlobalModuleType);

        // Pick the ParamPtr target before adding methods: only original rows
        // have low rids.
        var ret = mod.GetTypes().SelectMany(x => x.Methods)
            .Where(m => !m.ParamDefs.Any(p => p.Sequence == 0))
            .OrderBy(m => m.Rid)
            .FirstOrDefault();
        if (ret is null) {
            Console.Error.WriteLine("PtrGen: no method without a return-parameter row");
            return 1;
        }
        ret.ParamDefs.Insert(0, new ParamDefUser("ptr_ret", 0));

        t.Fields.Insert(0, new FieldDefUser("ptr_f", new FieldSig(cs.Int32), FieldAttributes.Public));

        var m = Method("ptr_m", MethodSig.CreateInstance(cs.Void, cs.Int32), Instance);
        m.ParamDefs.Add(new ParamDefUser("ptr_a", 1));
        t.Methods.Insert(0, m);

        var get = Method("get_PtrP", MethodSig.CreateInstance(cs.Int32), Accessor, OpCodes.Ldc_I4_0.ToInstruction());
        t.Methods.Add(get);
        t.Properties.Insert(0, new PropertyDefUser("PtrP", PropertySig.CreateInstance(cs.Int32)) { GetMethod = get });

        var handler = mod.Import(typeof(EventHandler));
        var add = Method("add_PtrE", MethodSig.CreateInstance(cs.Void, handler.ToTypeSig()), Accessor);
        t.Methods.Add(add);
        t.Events.Insert(0, new EventDefUser("PtrE", handler) { AddMethod = add });

        var opts = new ModuleWriterOptions(mod);
        opts.MetadataOptions.Flags |= MetadataFlags.PreserveRids;
        mod.Write(args[1], opts);
        return 0;
    }
}
