// FlowRun: runs every method of an assembly that it can call, on .NET, with
// random arguments, and prints what happened: the reference side of xtask's
// `cil:exec` check, which runs the same methods in an interpreter over
// cil-reader's MethodFlow and compares.
//
//     flow-run [--trials N] [--seed S] [--filter TEXT] <assembly>
//
// One JSON object per line, per (method, trial):
//
//     {"method": <token>, "name": "Ns.Type::Name", "trial": <n>,
//      "args": [V...], "statics": [[<field token>, V]...], "heap": [H...],
//      "outcome": "<canonical text>"}
//
// `args` (with `this` first for an instance method; a ref/out argument's
// pointee), `statics` (every static field of the module's types) and `heap`
// are the state before the call, which the interpreter starts from. V is a
// JSON number for an integer of any width (bool and char included), {"f": bits}
// or {"d": bits} for float32/float64, null, {"r": heap index} for an object,
// and {"s": [[field token, V]...]} for a struct. H is {"t": type name, "f":
// [[field token, V]...]} for an object, {"a": element code, "n": [lengths],
// "v": [V...]} for an array, {"b": type name, "v": V} for a boxed value.
//
// `outcome` is the state after the call in a canonical text both sides
// render (see Canon): the result or the thrown object, the arguments, the
// statics, then every object reachable from those, numbered in the order a
// breadth-first walk first reaches them. Objects show only the fields that
// types of this module declare, so runtime-created exceptions show none.
//
// Inputs are drawn from a Random seeded by (seed, method token, trial), so a
// run is reproducible.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

static class Program
{
    static int Main (string [] args)
    {
        int trials = 32;
        int seed = 1;
        string filter = null;
        string path = null;
        for (int i = 0; i < args.Length; i++) {
            switch (args [i]) {
            case "--trials": trials = int.Parse (args [++i], CultureInfo.InvariantCulture); break;
            case "--seed": seed = int.Parse (args [++i], CultureInfo.InvariantCulture); break;
            case "--filter": filter = args [++i]; break;
            default: path = args [i]; break;
            }
        }
        if (path == null) {
            Console.Error.WriteLine ("usage: flow-run [--trials N] [--seed S] [--filter TEXT] <assembly>");
            return 1;
        }

        var asm = Assembly.LoadFrom (Path.GetFullPath (path));
        var model = new Model (asm.ManifestModule);
        var stdout = Console.OpenStandardOutput ();
        foreach (var method in model.Methods) {
            string name = Model.TypeName (method.DeclaringType) + "::" + method.Name;
            if (filter != null && !name.Contains (filter))
                continue;
            for (int trial = 0; trial < trials; trial++) {
                var rng = new Random (unchecked (seed * 1000003 + method.MetadataToken * 31 + trial));
                var line = Trial (model, method, name, trial, rng);
                stdout.Write (line);
                stdout.WriteByte ((byte) '\n');
            }
        }
        stdout.Flush ();
        return 0;
    }

    static byte [] Trial (Model model, MethodInfo method, string name, int trial, Random rng)
    {
        var gen = new Gen (model, rng);
        foreach (var f in model.Statics)
            if (!f.IsInitOnly)
                f.SetValue (null, gen.Value (f.FieldType, 0));

        var ps = method.GetParameters ();
        // Exactly the declaring type: Invoke dispatches virtual methods, and
        // the method under test is this one, not an override.
        object receiver = method.IsStatic ? null : gen.Exact (method.DeclaringType, 0);
        var args = new object [ps.Length];
        for (int k = 0; k < ps.Length; k++)
            args [k] = gen.Value (Model.Unref (ps [k].ParameterType), 0);
        var original = (object []) args.Clone ();

        var buffer = new MemoryStream ();
        using (var w = new Utf8JsonWriter (buffer)) {
            w.WriteStartObject ();
            w.WriteNumber ("method", method.MetadataToken);
            w.WriteString ("name", name);
            w.WriteNumber ("trial", trial);
            var enc = new Encoder (model, w);
            w.WriteStartArray ("args");
            if (!method.IsStatic)
                enc.Value (receiver, method.DeclaringType);
            for (int k = 0; k < ps.Length; k++)
                enc.Value (args [k], Model.Unref (ps [k].ParameterType));
            w.WriteEndArray ();
            w.WriteStartArray ("statics");
            foreach (var f in model.Statics) {
                w.WriteStartArray ();
                w.WriteNumberValue (f.MetadataToken);
                enc.Value (f.GetValue (null), f.FieldType);
                w.WriteEndArray ();
            }
            w.WriteEndArray ();
            enc.Heap ();

            object result = null;
            Exception thrown = null;
            try {
                result = method.Invoke (receiver, args);
            } catch (TargetInvocationException e) {
                thrown = e.InnerException;
            }

            var canon = new Canon (model);
            var text = new StringBuilder ();
            if (thrown != null)
                text.Append ("throw ").Append (canon.Value (thrown, typeof (object)));
            else if (method.ReturnType == typeof (void))
                text.Append ("ret void");
            else
                text.Append ("ret ").Append (canon.Value (result, method.ReturnType));
            text.Append ('\n');
            if (!method.IsStatic)
                text.Append ("this ").Append (canon.Value (receiver, method.DeclaringType)).Append ('\n');
            for (int k = 0; k < ps.Length; k++) {
                var t = ps [k].ParameterType;
                text.Append ("arg").Append (k).Append (' ');
                if (!t.IsByRef)
                    text.Append (canon.Value (original [k], t));
                else if (thrown != null)
                    // Reflection copies a ref argument back only on a normal return.
                    text.Append ('-');
                else
                    text.Append (canon.Value (args [k], t.GetElementType ()));
                text.Append ('\n');
            }
            foreach (var f in model.Statics)
                text.Append ("static ").Append (f.MetadataToken.ToString ("x8")).Append (' ')
                    .Append (canon.Value (f.GetValue (null), f.FieldType)).Append ('\n');
            canon.Heap (text);
            w.WriteString ("outcome", text.ToString ());
            w.WriteEndObject ();
        }
        return buffer.ToArray ();
    }
}

/// What FlowRun knows of the module under test: its types, the methods it
/// can call, and the static fields it resets before every call.
sealed class Model
{
    public readonly Module Module;
    public readonly List<Type> Types;
    public readonly List<FieldInfo> Statics;
    public readonly List<MethodInfo> Methods;
    readonly Dictionary<Type, List<FieldInfo>> fields = new Dictionary<Type, List<FieldInfo>> ();

    const BindingFlags Declared = BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly;

    public Model (Module module)
    {
        Module = module;
        Type [] all;
        try {
            all = module.GetTypes ();
        } catch (ReflectionTypeLoadException e) {
            all = e.Types.Where (t => t != null).ToArray ();
        }
        Types = all.Where (t => !t.IsGenericTypeDefinition && !t.Name.StartsWith ("<"))
            .OrderBy (t => t.MetadataToken).ToList ();
        Statics = Types.SelectMany (t => t.GetFields (Declared | BindingFlags.Static))
            .Where (f => !f.IsLiteral)
            .OrderBy (f => f.MetadataToken).ToList ();
        Methods = Types.SelectMany (t => t.GetMethods (Declared | BindingFlags.Static | BindingFlags.Instance))
            .Where (Callable)
            .OrderBy (m => m.MetadataToken).ToList ();
    }

    bool Callable (MethodInfo m)
    {
        if (m.IsAbstract || m.IsGenericMethodDefinition || m.Name.StartsWith ("<") || m.GetMethodBody () == null)
            return false;
        if (!m.IsStatic && (m.DeclaringType.IsValueType || m.DeclaringType.IsAbstract))
            return false;
        if (m.ReturnType != typeof (void) && !Supported (m.ReturnType))
            return false;
        return m.GetParameters ().All (p => Supported (Unref (p.ParameterType)) && !p.ParameterType.IsPointer);
    }

    public bool Own (Type t) => t.Module == Module && !t.IsArray;

    public bool Supported (Type t)
    {
        if (t.IsPrimitive || t == typeof (object))
            return true;
        if (t.IsArray)
            return Supported (t.GetElementType ());
        return Own (t) && !t.IsGenericType && !t.IsInterface && !t.IsEnum && !t.IsPointer;
    }

    public static Type Unref (Type t) => t.IsByRef ? t.GetElementType () : t;

    public static string TypeName (Type t) => t.FullName.Replace ('+', '/');

    /// The instance fields an object of `t` shows: those that types of this
    /// module declare, in token order.
    public List<FieldInfo> Fields (Type t)
    {
        if (fields.TryGetValue (t, out var list))
            return list;
        list = new List<FieldInfo> ();
        for (var c = t; c != null; c = c.BaseType)
            if (Own (c))
                list.AddRange (c.GetFields (Declared | BindingFlags.Instance));
        list.Sort ((a, b) => a.MetadataToken.CompareTo (b.MetadataToken));
        fields [t] = list;
        return list;
    }

    /// The module's concrete classes assignable to `t`. Not delegate types: a delegate is
    /// made by binding a method, and one made without a constructor is not a delegate.
    public List<Type> Concrete (Type t) =>
        Types.Where (c => c.IsClass && !c.IsAbstract && !typeof (Delegate).IsAssignableFrom (c)
            && t.IsAssignableFrom (c)).ToList ();

    /// The element code of an array's element type.
    public static string ElementCode (Type t)
    {
        switch (Type.GetTypeCode (t)) {
        case TypeCode.Boolean: return "bool";
        case TypeCode.Char: return "char";
        case TypeCode.SByte: return "i1";
        case TypeCode.Byte: return "u1";
        case TypeCode.Int16: return "i2";
        case TypeCode.UInt16: return "u2";
        case TypeCode.Int32: return "i4";
        case TypeCode.UInt32: return "u4";
        case TypeCode.Int64: return "i8";
        case TypeCode.UInt64: return "u8";
        case TypeCode.Single: return "r4";
        case TypeCode.Double: return "r8";
        }
        if (t == typeof (IntPtr))
            return "i";
        if (t == typeof (UIntPtr))
            return "u";
        if (t.IsValueType)
            return "vt:" + TypeName (t);
        return "ref:" + TypeName (t);
    }
}

/// Random inputs, biased toward small values and the edges of each type.
sealed class Gen
{
    readonly Model model;
    readonly Random rng;
    const int MaxDepth = 3;

    public Gen (Model model, Random rng)
    {
        this.model = model;
        this.rng = rng;
    }

    long Integer (long min, long max)
    {
        int r = rng.Next (100);
        long v;
        if (r < 40)
            v = rng.Next (-8, 9);
        else if (r < 55)
            v = rng.Next (0, 200);
        else if (r < 75) {
            long [] edges = { 0, 1, -1, 2, 3, 4, 5, 7, min, max, min + 1, max - 1, 100, 1000 };
            v = edges [rng.Next (edges.Length)];
        } else
            v = rng.NextInt64 ();
        return v;
    }

    double Real (bool single)
    {
        int r = rng.Next (100);
        if (r < 30)
            return rng.Next (-8, 9);
        if (r < 55)
            return Math.Round (rng.NextDouble () * 20 - 10, 3);
        if (r < 75) {
            double [] edges = { 0.0, -0.0, double.NaN, double.PositiveInfinity, double.NegativeInfinity,
                0.5, -0.5, 1e10, -3e9, 4.5e9, 1e300, double.Epsilon, float.MaxValue, float.Epsilon, 2147483647.5 };
            return edges [rng.Next (edges.Length)];
        }
        if (single)
            return BitConverter.Int32BitsToSingle (rng.Next () ^ (rng.Next (2) << 31));
        return BitConverter.Int64BitsToDouble (rng.NextInt64 () ^ ((long) rng.Next (2) << 63));
    }

    public object Value (Type t, int depth)
    {
        switch (Type.GetTypeCode (t)) {
        case TypeCode.Boolean: return rng.Next (2) == 1;
        case TypeCode.Char: return (char) (rng.Next (3) == 0 ? rng.Next (0x10000) : rng.Next (32, 128));
        case TypeCode.SByte: return unchecked ((sbyte) Integer (sbyte.MinValue, sbyte.MaxValue));
        case TypeCode.Byte: return unchecked ((byte) Integer (byte.MinValue, byte.MaxValue));
        case TypeCode.Int16: return unchecked ((short) Integer (short.MinValue, short.MaxValue));
        case TypeCode.UInt16: return unchecked ((ushort) Integer (ushort.MinValue, ushort.MaxValue));
        case TypeCode.Int32: return unchecked ((int) Integer (int.MinValue, int.MaxValue));
        case TypeCode.UInt32: return unchecked ((uint) Integer (uint.MinValue, uint.MaxValue));
        case TypeCode.Int64: return Integer (long.MinValue, long.MaxValue);
        case TypeCode.UInt64: return unchecked ((ulong) Integer (0, long.MaxValue) * (ulong) (rng.Next (2) + 1));
        case TypeCode.Single: return (float) Real (true);
        case TypeCode.Double: return Real (false);
        }
        if (t == typeof (IntPtr))
            return (IntPtr) Integer (long.MinValue, long.MaxValue);
        if (t == typeof (UIntPtr))
            return (UIntPtr) unchecked ((ulong) Integer (0, long.MaxValue));
        if (t.IsValueType)
            return Fill (RuntimeHelpers.GetUninitializedObject (t), depth);
        if (depth >= MaxDepth || rng.Next (8) == 0)
            return null;
        if (t.IsArray) {
            var e = t.GetElementType ();
            var a = Array.CreateInstance (e, rng.Next (0, 6));
            for (int i = 0; i < a.Length; i++)
                a.SetValue (Value (e, depth + 1), i);
            return a;
        }
        if (t == typeof (object)) {
            switch (rng.Next (4)) {
            case 0: return Value (typeof (int), depth);
            case 1: return Value (typeof (long), depth);
            case 2: return Value (typeof (int []), depth);
            }
        }
        var concrete = model.Concrete (t);
        if (concrete.Count == 0)
            return null;
        return Exact (concrete [rng.Next (concrete.Count)], depth);
    }

    /// A non-null object of exactly class `t`.
    public object Exact (Type t, int depth) =>
        Fill (RuntimeHelpers.GetUninitializedObject (t), depth);

    object Fill (object o, int depth)
    {
        foreach (var f in model.Fields (o.GetType ()))
            f.SetValue (o, Value (f.FieldType, depth + 1));
        return o;
    }
}

/// Writes the initial state as JSON: values inline, objects into a heap
/// numbered in the order they are first reached.
sealed class Encoder
{
    readonly Model model;
    readonly Utf8JsonWriter w;
    readonly Dictionary<object, int> ids = new Dictionary<object, int> (ReferenceEqualityComparer.Instance);
    readonly List<object> objects = new List<object> ();

    public Encoder (Model model, Utf8JsonWriter w)
    {
        this.model = model;
        this.w = w;
    }

    public void Value (object v, Type t)
    {
        switch (Type.GetTypeCode (t)) {
        case TypeCode.Boolean: w.WriteNumberValue ((bool) v ? 1 : 0); return;
        case TypeCode.Char: w.WriteNumberValue ((int) (char) v); return;
        case TypeCode.SByte: w.WriteNumberValue ((sbyte) v); return;
        case TypeCode.Byte: w.WriteNumberValue ((byte) v); return;
        case TypeCode.Int16: w.WriteNumberValue ((short) v); return;
        case TypeCode.UInt16: w.WriteNumberValue ((ushort) v); return;
        case TypeCode.Int32: w.WriteNumberValue ((int) v); return;
        case TypeCode.UInt32: w.WriteNumberValue ((uint) v); return;
        case TypeCode.Int64: w.WriteNumberValue ((long) v); return;
        case TypeCode.UInt64: w.WriteNumberValue ((ulong) v); return;
        case TypeCode.Single:
            w.WriteStartObject ();
            w.WriteNumber ("f", unchecked ((uint) BitConverter.SingleToInt32Bits ((float) v)));
            w.WriteEndObject ();
            return;
        case TypeCode.Double:
            w.WriteStartObject ();
            w.WriteNumber ("d", unchecked ((ulong) BitConverter.DoubleToInt64Bits ((double) v)));
            w.WriteEndObject ();
            return;
        }
        if (t == typeof (IntPtr)) {
            w.WriteNumberValue ((long) (IntPtr) v);
            return;
        }
        if (t == typeof (UIntPtr)) {
            w.WriteNumberValue ((ulong) (UIntPtr) v);
            return;
        }
        if (t.IsValueType) {
            w.WriteStartObject ();
            Fields ("s", v);
            w.WriteEndObject ();
            return;
        }
        if (v == null) {
            w.WriteNullValue ();
            return;
        }
        if (!ids.TryGetValue (v, out int id)) {
            id = objects.Count;
            ids [v] = id;
            objects.Add (v);
        }
        w.WriteStartObject ();
        w.WriteNumber ("r", id);
        w.WriteEndObject ();
    }

    void Fields (string name, object o)
    {
        w.WriteStartArray (name);
        foreach (var f in model.Fields (o.GetType ())) {
            w.WriteStartArray ();
            w.WriteNumberValue (f.MetadataToken);
            Value (f.GetValue (o), f.FieldType);
            w.WriteEndArray ();
        }
        w.WriteEndArray ();
    }

    /// The heap: every object reached so far, and those they reach.
    public void Heap ()
    {
        w.WriteStartArray ("heap");
        for (int i = 0; i < objects.Count; i++) {
            var o = objects [i];
            var t = o.GetType ();
            w.WriteStartObject ();
            if (t.IsArray) {
                var a = (Array) o;
                var e = t.GetElementType ();
                w.WriteString ("a", Model.ElementCode (e));
                w.WriteStartArray ("n");
                for (int d = 0; d < a.Rank; d++)
                    w.WriteNumberValue (a.GetLength (d));
                w.WriteEndArray ();
                w.WriteStartArray ("v");
                foreach (var x in a)
                    Value (x, e);
                w.WriteEndArray ();
            } else if (t.IsValueType) {
                w.WriteString ("b", Model.TypeName (t));
                w.WritePropertyName ("v");
                Value (o, t);
            } else {
                w.WriteString ("t", Model.TypeName (t));
                Fields ("f", o);
            }
            w.WriteEndObject ();
        }
        w.WriteEndArray ();
    }
}

/// The canonical text of a state, which the interpreter renders the same
/// way: integers in decimal, floats as bit patterns (any NaN as NaN), structs
/// inline, objects as @n with n the order a breadth-first walk from the
/// roots first reaches them.
sealed class Canon
{
    readonly Model model;
    readonly Dictionary<object, int> ids = new Dictionary<object, int> (ReferenceEqualityComparer.Instance);
    readonly List<object> objects = new List<object> ();

    public Canon (Model model)
    {
        this.model = model;
    }

    public string Value (object v, Type t)
    {
        var ci = CultureInfo.InvariantCulture;
        switch (Type.GetTypeCode (t)) {
        case TypeCode.Boolean: return (bool) v ? "1" : "0";
        case TypeCode.Char: return ((int) (char) v).ToString (ci);
        case TypeCode.SByte: return ((sbyte) v).ToString (ci);
        case TypeCode.Byte: return ((byte) v).ToString (ci);
        case TypeCode.Int16: return ((short) v).ToString (ci);
        case TypeCode.UInt16: return ((ushort) v).ToString (ci);
        case TypeCode.Int32: return ((int) v).ToString (ci);
        case TypeCode.UInt32: return ((uint) v).ToString (ci);
        case TypeCode.Int64: return ((long) v).ToString (ci);
        case TypeCode.UInt64: return ((ulong) v).ToString (ci);
        case TypeCode.Single: {
            float f = (float) v;
            return float.IsNaN (f) ? "fNaN" : "f" + BitConverter.SingleToInt32Bits (f).ToString ("x8");
        }
        case TypeCode.Double: {
            double d = (double) v;
            return double.IsNaN (d) ? "dNaN" : "d" + BitConverter.DoubleToInt64Bits (d).ToString ("x16");
        }
        }
        if (t == typeof (IntPtr))
            return ((long) (IntPtr) v).ToString (ci);
        if (t == typeof (UIntPtr))
            return ((ulong) (UIntPtr) v).ToString (ci);
        if (t.IsValueType)
            return "{" + Fields (v) + "}";
        if (v == null)
            return "null";
        if (!ids.TryGetValue (v, out int id)) {
            id = objects.Count;
            ids [v] = id;
            objects.Add (v);
        }
        return "@" + id.ToString (ci);
    }

    string Fields (object o) =>
        string.Join (",", model.Fields (o.GetType ()).Select (f =>
            f.MetadataToken.ToString ("x8") + "=" + Value (f.GetValue (o), f.FieldType)));

    public void Heap (StringBuilder text)
    {
        for (int i = 0; i < objects.Count; i++) {
            var o = objects [i];
            var t = o.GetType ();
            text.Append ('@').Append (i.ToString (CultureInfo.InvariantCulture)).Append (' ');
            if (t.IsArray) {
                var a = (Array) o;
                var e = t.GetElementType ();
                text.Append (Model.ElementCode (e)).Append ('[');
                for (int d = 0; d < a.Rank; d++)
                    text.Append (d == 0 ? "" : "x").Append (a.GetLength (d).ToString (CultureInfo.InvariantCulture));
                text.Append ("] ");
                var items = new List<string> ();
                foreach (var x in a)
                    items.Add (Value (x, e));
                text.Append (string.Join (",", items));
            } else if (t.IsValueType) {
                text.Append ("box ").Append (Model.TypeName (t)).Append (' ').Append (Value (o, t));
            } else {
                text.Append (Model.TypeName (t)).Append (" {").Append (Fields (o)).Append ('}');
            }
            text.Append ('\n');
        }
    }
}
