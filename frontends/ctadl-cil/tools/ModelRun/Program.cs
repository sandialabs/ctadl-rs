// ModelRun: calls .NET library methods with marked arguments and prints which
// inputs reached which outputs, the reference side of xtask's `cil:models`
// check of the default models (`defaults/cil-index.jsonl`).
//
//     model-run list <classes-file>
//     model-run run [--trials N] [--seed S] <fids-file>
//
// `list` reads one class per line in ctadl's spelling (`[Asm]Ns.Outer/Inner`1`,
// as the frontend's `names.rs` defines it) and prints one JSON object per public
// method and constructor the class declares, spelled as the CIL frontend
// spells it:
//
//     {"cls": ..., "simple": ..., "desc": ..., "fid": ..., "arity": n, "kind": "static"|"instance"|"ctor"}
//
// or {"cls": ..., "error": ...} for a class it cannot load, or that lives in
// another assembly than the one named (the frontend canonicalizes through
// forwarders, so a model must name the assembly that defines the type).
//
// `run` reads one function id per line, as `list` printed them, and prints one
// JSON object per id:
//
//     {"fid": ..., "trials": n, "ran": n, "threw": {"<exception>": n}, "skip": "<why>",
//      "flows": [{"from": [[port, [segment...]]...], "to": [port, [segment...]]}...],
//      "depends": [[from, to]...]}
//
// Ports are numbered as a model numbers them: `Argument(n)` is n, with the
// receiver (or the object a constructor makes) at 0, and `Return` is -1. A
// path is a list of segments, `["deref", "[]"]`.
//
// Run it in an empty directory: a library call may take a marker string for a
// file name. It never builds a file stream itself.
//
// A flow is data: a marker string placed in an input was found in an output
// it was not in before the call. `from` lists every input location that held
// the marker, since a trial may pass one string twice (a dictionary key, say);
// one of them is the source. A dependence is weaker: changing only that input
// changed what the output holds, which a branch on the input does too.
//
// Paths are the models' abstraction of a value, not the library's fields: `[]`
// is an element of an array or of anything enumerable (a dictionary's keys and
// values alike), `deref` is what a `ref`/`out` parameter, or a struct
// receiver, points at, and everything else inside an object is the object
// itself. Inputs are labeled by the same walk that reads outputs, so both
// sides always agree on where a marker is.
//
// Inputs are drawn from a Random seeded by (seed, id, trial, argument), so a
// run is reproducible.

using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Linq.Expressions;
using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;

static class Program
{
    static int Main (string [] args)
    {
        int trials = 32;
        int seed = 1;
        string mode = null;
        string path = null;
        for (int i = 0; i < args.Length; i++) {
            switch (args [i]) {
            case "--trials": trials = int.Parse (args [++i], CultureInfo.InvariantCulture); break;
            case "--seed": seed = int.Parse (args [++i], CultureInfo.InvariantCulture); break;
            default:
                if (mode == null)
                    mode = args [i];
                else
                    path = args [i];
                break;
            }
        }
        if (path == null || (mode != "list" && mode != "run")) {
            Console.Error.WriteLine ("usage: model-run list <classes-file>");
            Console.Error.WriteLine ("       model-run run [--trials N] [--seed S] <fids-file>");
            return 1;
        }
        var lines = File.ReadAllLines (path).Select (l => l.Trim ()).Where (l => l.Length > 0).ToList ();
        var output = Console.Out;
        if (mode == "list") {
            foreach (var cls in lines)
                List (cls, output);
        } else {
            var methods = new Dictionary<string, MethodBase> ();
            foreach (var cls in lines.Select (Names.ClassOf).Distinct ()) {
                var type = Names.Resolve (cls, out _);
                if (type == null)
                    continue;
                foreach (var m in Names.Members (type))
                    methods [Names.Fid (m)] = m;
            }
            foreach (var fid in lines) {
                methods.TryGetValue (fid, out var m);
                // A call can block (taking from an empty BlockingCollection); it is abandoned on a
                // background thread rather than holding up the rest.
                string row = null;
                var thread = new Thread (() => row = Runner.Run (fid, m, trials, seed)) { IsBackground = true };
                thread.Start ();
                if (!thread.Join (TimeSpan.FromSeconds (30)))
                    row = JsonSerializer.Serialize (new Dictionary<string, object> { ["fid"] = fid, ["trials"] = trials, ["skip"] = "timed out" });
                output.WriteLine (row);
            }
        }
        output.Flush ();
        return 0;
    }

    static void List (string cls, TextWriter output)
    {
        var type = Names.Resolve (cls, out var error);
        if (type == null) {
            output.WriteLine (JsonSerializer.Serialize (new Dictionary<string, object> { ["cls"] = cls, ["error"] = error }));
            return;
        }
        foreach (var m in Names.Members (type)) {
            var row = new Dictionary<string, object> {
                ["cls"] = cls,
                ["simple"] = Names.Simple (m),
                ["desc"] = Names.Descriptor (m),
                ["fid"] = Names.Fid (m),
                ["arity"] = m.GetParameters ().Length + (m.IsStatic ? 0 : 1),
                ["kind"] = m is ConstructorInfo ? "ctor" : m.IsStatic ? "static" : "instance",
            };
            output.WriteLine (JsonSerializer.Serialize (row));
        }
    }
}

// The CIL frontend's spellings (`frontends/ctadl-cil/src/names.rs`), from reflection.
static class Names
{
    public static string ClassOf (string fid)
    {
        int i = fid.IndexOf ("::", StringComparison.Ordinal);
        return i < 0 ? fid : fid.Substring (0, i);
    }

    public static Type Resolve (string cls, out string error)
    {
        error = null;
        var m = Regex.Match (cls, @"^\[([^\]]+)\](.+)$");
        if (!m.Success) {
            error = "not of the form [Assembly]Namespace.Type";
            return null;
        }
        string asm = m.Groups [1].Value, full = m.Groups [2].Value.Replace ('/', '+');
        Type type;
        try {
            type = Type.GetType (full + ", " + asm, false);
        } catch (Exception e) {
            error = e.GetType ().Name + ": " + e.Message;
            return null;
        }
        if (type == null) {
            error = "no such type";
            return null;
        }
        string actual = type.Assembly.GetName ().Name;
        if (actual != asm) {
            error = "defined in [" + actual + "]";
            return null;
        }
        return type;
    }

    const BindingFlags Declared = BindingFlags.Public | BindingFlags.Instance | BindingFlags.Static | BindingFlags.DeclaredOnly;

    public static IEnumerable<MethodBase> Members (Type type)
    {
        foreach (var c in type.GetConstructors (BindingFlags.Public | BindingFlags.Instance))
            yield return c;
        foreach (var m in type.GetMethods (Declared))
            yield return m;
    }

    public static string Cls (Type t)
    {
        if (t.IsGenericType)
            t = t.GetGenericTypeDefinition ();
        return "[" + t.Assembly.GetName ().Name + "]" + Full (t);
    }

    static string Full (Type t) => t.FullName.Replace ('+', '/');

    public static string Simple (MethodBase m)
    {
        if (m is MethodInfo mi && mi.IsGenericMethodDefinition)
            return m.Name + "``" + mi.GetGenericArguments ().Length;
        return m.Name;
    }

    public static string Descriptor (MethodBase m)
    {
        var sb = new StringBuilder ("(");
        sb.Append (string.Join (",", m.GetParameters ().Select (p => Spell (p.ParameterType))));
        if ((m.CallingConvention & CallingConventions.VarArgs) != 0)
            sb.Append (m.GetParameters ().Length > 0 ? ",..." : "...");
        sb.Append (")->");
        sb.Append (m is MethodInfo mi ? Spell (mi.ReturnType) : "System.Void");
        return sb.ToString ();
    }

    public static string Fid (MethodBase m) => Cls (m.DeclaringType) + "::" + Simple (m) + Descriptor (m);

    // A type inside a descriptor: Cecil's full names, generic parameters by position, no
    // assembly.
    public static string Spell (Type t)
    {
        if (t.IsByRef)
            return Spell (t.GetElementType ()) + "&";
        if (t.IsPointer)
            return Spell (t.GetElementType ()) + "*";
        if (t.IsArray)
            return Spell (t.GetElementType ()) + (t.IsSZArray ? "[]" : "[" + new string (',', t.GetArrayRank () - 1) + "]");
        if (t.IsGenericParameter)
            return (t.DeclaringMethod != null ? "!!" : "!") + t.GenericParameterPosition;
        if (t.IsFunctionPointer)
            return "method ?";
        if (t.IsGenericType) {
            // A generic type used inside its own definition comes back from reflection as the
            // definition itself; its arguments are then its own parameters, which spell as `!n`.
            var def = t.GetGenericTypeDefinition ();
            return Full (def) + "<" + string.Join (",", t.GetGenericArguments ().Select (Spell)) + ">";
        }
        return Full (t);
    }
}

// One location of a marker: a port and a path within it.
readonly record struct Loc (int Port, string Path);

// Walks a value the way the models see it, collecting the markers in it and a canonical text.
sealed class Walker
{
    public static readonly Regex Marker = new Regex (@"zq(\d+)x", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);

    public readonly HashSet<(string Path, int Marker)> Hits = new ();
    public readonly StringBuilder Text = new ();
    readonly HashSet<object> seen = new (ReferenceEqualityComparer.Instance);
    int budget = 20000;

    // Segments are joined with a separator no field name contains: a field symbol is
    // `<[Asm]Ns.Type::name>`, dots and all.
    public const char Sep = '\u001f';

    public static string Seg (string path, string seg) => path.Length == 0 ? seg : path + Sep + seg;

    // `field` says `o` was reached through a field of an opaque object, where an array is that
    // object's own storage (a reader's buffer, a document's bytes), not elements of it. `flat`
    // says `o` is already inside an element, so nothing below it adds a level: an enumerator's
    // fields, say, which hold the collection it walks and the element it is on.
    public void Visit (object o, string path, int depth = 0, bool field = false, bool flat = false)
    {
        string Elem () => flat ? path : Seg (path, "[]");
        if (o == null) {
            Text.Append (path).Append ("=null;");
            return;
        }
        if (--budget < 0 || depth > 10)
            return;
        var t = o.GetType ();
        switch (o) {
        case string s: Leaf (path, s); return;
        case char [] cs: Leaf (field ? path : Elem (), new string (cs)); return;
        case byte [] bs: Leaf (field ? path : Elem (), Encoding.UTF8.GetString (bs)); return;
        }
        if (t.IsPrimitive || t.IsEnum || o is decimal) {
            Text.Append (path).Append ('=').Append (Convert.ToString (o, CultureInfo.InvariantCulture)).Append (';');
            return;
        }
        if (o is Delegate || o is MemberInfo || o is Assembly || o is Module || o is CultureInfo || o is Encoding
            || o is Pointer || o is Thread || o is CancellationTokenSource || o is IFormatProvider)
            return;
        if (!t.IsValueType && !seen.Add (o))
            return;
        switch (o) {
        case StringBuilder sb: Leaf (path, sb.ToString ()); return;
        case StringWriter sw: Leaf (path, sw.ToString ()); return;
        case MemoryStream ms:
            try { Leaf (path, Encoding.UTF8.GetString (ms.ToArray ())); } catch (ObjectDisposedException) { }
            return;
        case Array arr:
            int k = 0;
            foreach (var e in arr) {
                if (k++ >= 64)
                    break;
                Visit (e, field ? path : Elem (), depth + 1, field, flat);
            }
            return;
        case System.Collections.Specialized.NameValueCollection nv:
            // Enumerates its keys only; a model's element is a key or a value.
            foreach (string key in nv.AllKeys.Take (64)) {
                Visit (key, Elem (), depth + 1, flat: flat);
                Visit (nv [key], Elem (), depth + 1, flat: flat);
            }
            return;
        case System.Text.RegularExpressions.Capture c:
            // A match, a group or a capture is the text it matched, not the input it holds.
            Leaf (path, c.Value);
            return;
        case IEnumerable en when Enumerable (t):
            try {
                int n = 0;
                foreach (var e in en) {
                    if (n++ >= 64)
                        break;
                    Visit (e, Elem (), depth + 1, flat: flat);
                }
            } catch (Exception) {
                Text.Append (path).Append ("=!enumerate;");
            }
            return;
        }
        if (t.IsGenericType) {
            var def = t.GetGenericTypeDefinition ();
            // A ValueTuple's fields are public and C# reads them directly, so a model names them;
            // one a library keeps inside an object is that object's.
            if (!field && !flat && t.FullName.StartsWith ("System.ValueTuple`", StringComparison.Ordinal)) {
                foreach (var f in t.GetFields (BindingFlags.Instance | BindingFlags.Public))
                    Visit (f.GetValue (o), Seg (path, "<" + Names.Cls (t) + "::" + f.Name + ">"), depth + 1);
                return;
            }
            // A memory is a view of its array's elements, as an enumerable is.
            if (def == typeof (Memory<>) || def == typeof (ReadOnlyMemory<>)) {
                Visit (t.GetMethod ("ToArray").Invoke (o, null), path, depth + 1, field, flat);
                return;
            }
            // A node holds its list, but it is only its value.
            if (def == typeof (LinkedListNode<>)) {
                Visit (t.GetProperty ("Value").GetValue (o), path, depth + 1, field, flat);
                return;
            }
        }
        // An enumerator is a view of the elements it walks: whatever it holds is an element.
        if (o is IEnumerator) {
            for (var c = t; c != null && c != typeof (object) && c != typeof (ValueType); c = c.BaseType)
                foreach (var f in c.GetFields (BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly)) {
                    if (f.FieldType.IsPointer || f.FieldType.IsByRefLike || f.FieldType.IsFunctionPointer)
                        continue;
                    object v;
                    try { v = f.GetValue (o); } catch (Exception) { continue; }
                    Visit (v, Elem (), depth + 1, true, true);
                }
            return;
        }
        // Anything else is opaque: what its fields hold is the object itself.
        for (var c = t; c != null && c != typeof (object) && c != typeof (ValueType); c = c.BaseType) {
            foreach (var f in c.GetFields (BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly)) {
                if (f.FieldType.IsPointer || f.FieldType.IsByRefLike || f.FieldType.IsFunctionPointer)
                    continue;
                object v;
                try { v = f.GetValue (o); } catch (Exception) { continue; }
                Visit (v, path, depth + 1, true, flat);
            }
        }
    }

    // A stream's or a reader's contents would be consumed by reading them; their fields are walked
    // instead. Everything else that enumerates is a collection or a lazy view of one, including a
    // LINQ iterator, which is its own enumerator too.
    static bool Enumerable (Type t) => !typeof (Stream).IsAssignableFrom (t) && !typeof (TextReader).IsAssignableFrom (t);

    void Leaf (string path, string s)
    {
        Text.Append (path).Append ("=\"").Append (s).Append ("\";");
        foreach (Match m in Marker.Matches (s))
            Hits.Add ((path, int.Parse (m.Groups [1].Value, CultureInfo.InvariantCulture)));
    }
}

// Builds argument values. Every marker it writes is `zq<n>x` for an `n` fixed by (trial,
// argument, variant), so two runs that differ in one argument agree on all the others.
sealed class Gen
{
    readonly Random rng;
    readonly int markerBase;
    int seq;
    readonly List<string> pool;

    public Gen (Random rng, int markerBase, List<string> pool)
    {
        this.rng = rng;
        this.markerBase = markerBase;
        this.pool = pool;
    }

    string NextMarker () => "zq" + (markerBase + seq++).ToString (CultureInfo.InvariantCulture) + "x";

    // Most shapes carry a marker; the rest are what a parser accepts, which only a dependence sees.
    enum Shape { Plain, Digits, Uri, JsonObject, JsonString, Base64, Bool, Char, Date, Guid }

    public string String ()
    {
        if (pool != null && pool.Count > 0 && rng.Next (2) == 0)
            return pool [rng.Next (pool.Count)];
        string m = NextMarker ();
        switch ((Shape) rng.Next (10)) {
        case Shape.Digits: return rng.Next (1, 100).ToString (CultureInfo.InvariantCulture);
        case Shape.Bool: return rng.Next (2) == 0 ? "true" : "False";
        case Shape.Char: return ((char) ('a' + rng.Next (26))).ToString ();
        case Shape.Date: return "2020-0" + rng.Next (1, 10) + "-1" + rng.Next (0, 10);
        case Shape.Guid: return new Guid (rng.Next (), 1, 2, new byte [8]).ToString ();
        case Shape.Uri: return "http://h.example/a" + m + "?q=" + m + "b";
        case Shape.JsonObject: return "{\"k" + m + "\":\"" + m + "\"}";
        case Shape.JsonString: return "\"" + m + "\"";
        case Shape.Base64: return Convert.ToBase64String (Encoding.UTF8.GetBytes ("pad" + m + "pad"));
        default: return "abcd" + m + "efgh";
        }
    }

    public object Value (Type t, int depth = 0)
    {
        if (t.IsByRef)
            t = t.GetElementType ();
        if (t == typeof (string))
            return String ();
        if (t == typeof (object))
            return rng.Next (3) == 0 ? rng.Next (0, 4) : String ();
        if (t == typeof (bool))
            return rng.Next (2) == 0;
        if (t == typeof (char))
            return (char) ('a' + rng.Next (4));
        if (t.IsEnum) {
            var values = Enum.GetValues (t);
            return values.Length == 0 ? Activator.CreateInstance (t) : values.GetValue (rng.Next (values.Length));
        }
        if (t.IsPrimitive || t == typeof (decimal))
            return Convert.ChangeType (rng.Next (0, 4), t, CultureInfo.InvariantCulture);
        // Scalars above always get a value; only what nests is cut off.
        if (depth > 4)
            return null;
        if (t == typeof (char []))
            return String ().ToCharArray ();
        if (t == typeof (byte []))
            return Encoding.UTF8.GetBytes (String ());
        if (t == typeof (Array))
            t = typeof (string []);
        if (t.IsArray) {
            var e = t.GetElementType ();
            var arr = Array.CreateInstance (e, rng.Next (1, 5));
            for (int i = 0; i < arr.Length; i++)
                arr.SetValue (Value (e, depth + 1), i);
            return arr;
        }
        if (typeof (Delegate).IsAssignableFrom (t))
            return Delegate (t);
        if (t == typeof (IFormatProvider) || t == typeof (CultureInfo))
            return CultureInfo.InvariantCulture;
        if (t == typeof (Encoding))
            return Encoding.UTF8;
        if (t == typeof (Type))
            return typeof (string);
        if (t == typeof (CancellationToken))
            return CancellationToken.None;
        if (t == typeof (StringComparer))
            return StringComparer.Ordinal;
        if (t == typeof (Uri))
            return new Uri ("http://h.example/a" + NextMarker () + "?q=" + NextMarker ());
        if (t == typeof (StringBuilder))
            return new StringBuilder (String ());
        if (t == typeof (Stream) || t == typeof (MemoryStream)) {
            var ms = new MemoryStream ();
            var bytes = Encoding.UTF8.GetBytes (String ());
            ms.Write (bytes, 0, bytes.Length);
            ms.Position = 0;
            return ms;
        }
        if (t == typeof (TextReader) || t == typeof (StringReader))
            return new StringReader (String ());
        if (t == typeof (StreamReader))
            return new StreamReader ((Stream) Value (typeof (Stream), depth + 1));
        // Nothing here may touch the file system: a marker would become a file name.
        if (t == typeof (FileStream) || t == typeof (FileInfo) || t == typeof (DirectoryInfo))
            return null;
        if (t == typeof (StreamWriter))
            return new StreamWriter (new MemoryStream ());
        if (t == typeof (BinaryReader)) {
            var ms = new MemoryStream ();
            using (var w = new BinaryWriter (ms, Encoding.UTF8, true)) {
                w.Write (String ());
                w.Write (String ());
            }
            ms.Position = 0;
            return new BinaryReader (ms);
        }
        if (t == typeof (System.Collections.IDictionary) || t == typeof (System.Collections.Hashtable)) {
            var h = new System.Collections.Hashtable ();
            for (int i = rng.Next (1, 4); i > 0; i--)
                h [String ()] = String ();
            return h;
        }
        if (t == typeof (System.Collections.IDictionaryEnumerator))
            return Started (((System.Collections.Hashtable) Value (typeof (System.Collections.Hashtable), depth + 1)).GetEnumerator ());
        if (t == typeof (System.Text.CompositeFormat))
            return System.Text.CompositeFormat.Parse (String () + "{0}");
        if (t == typeof (TextWriter) || t == typeof (StringWriter)) {
            var sw = new StringWriter (CultureInfo.InvariantCulture);
            sw.Write (String ());
            return sw;
        }
        if (t == typeof (System.Text.RegularExpressions.Match))
            return System.Text.RegularExpressions.Regex.Match (String (), "(.)(.+)");
        if (t == typeof (System.Text.RegularExpressions.Group) || t == typeof (System.Text.RegularExpressions.Capture))
            return ((System.Text.RegularExpressions.Match) Value (typeof (System.Text.RegularExpressions.Match), depth + 1)).Groups [2];
        if (t == typeof (System.Text.RegularExpressions.GroupCollection))
            return ((System.Text.RegularExpressions.Match) Value (typeof (System.Text.RegularExpressions.Match), depth + 1)).Groups;
        if (t == typeof (System.Text.RegularExpressions.MatchCollection))
            return System.Text.RegularExpressions.Regex.Matches (String (), ".+");
        if (t == typeof (JsonDocument))
            return JsonDocument.Parse ("{\"k\":\"" + NextMarker () + "\",\"a\":[\"" + NextMarker () + "\"]}");
        if (t == typeof (JsonElement))
            return ((JsonDocument) Value (typeof (JsonDocument), depth + 1)).RootElement;
        if (t.IsGenericType) {
            var def = t.GetGenericTypeDefinition ();
            var a = t.GetGenericArguments ();
            if (def == typeof (Nullable<>))
                return Value (a [0], depth + 1);
            if (def == typeof (KeyValuePair<,>))
                return Activator.CreateInstance (t, Value (a [0], depth + 1), Value (a [1], depth + 1));
            if (def == typeof (Task<>))
                return typeof (Task).GetMethod ("FromResult").MakeGenericMethod (a).Invoke (null, new [] { Value (a [0], depth + 1) });
            if (def == typeof (ValueTask<>))
                return Activator.CreateInstance (t, Value (a [0], depth + 1));
            if (def == typeof (System.Runtime.CompilerServices.TaskAwaiter<>)) {
                var task = Value (typeof (Task<>).MakeGenericType (a), depth + 1);
                return task.GetType ().GetMethod ("GetAwaiter").Invoke (task, null);
            }
            if (def == typeof (System.Runtime.CompilerServices.ConfiguredTaskAwaitable<>)) {
                var task = Value (typeof (Task<>).MakeGenericType (a), depth + 1);
                return task.GetType ().GetMethod ("ConfigureAwait", new [] { typeof (bool) }).Invoke (task, new object [] { false });
            }
            if (t.DeclaringType != null && t.DeclaringType.IsGenericType
                && t.DeclaringType.GetGenericTypeDefinition () == typeof (System.Runtime.CompilerServices.ConfiguredTaskAwaitable<>)) {
                var awaitable = Value (typeof (System.Runtime.CompilerServices.ConfiguredTaskAwaitable<>).MakeGenericType (a), depth + 1);
                return awaitable.GetType ().GetMethod ("GetAwaiter").Invoke (awaitable, null);
            }
            if (def == typeof (IOrderedEnumerable<>)) {
                var list = Value (typeof (List<>).MakeGenericType (a), depth + 1);
                var key = typeof (Func<,>).MakeGenericType (a [0], a [0]);
                return typeof (Enumerable).GetMethods ().First (m => m.Name == "OrderBy" && m.GetParameters ().Length == 2)
                    .MakeGenericMethod (a [0], a [0]).Invoke (null, new [] { list, Delegate (key) });
            }
            if (def == typeof (IEnumerator<>)) {
                var list = Value (typeof (List<>).MakeGenericType (a), depth + 1);
                return Started (((IEnumerable) list).GetEnumerator ());
            }
            if (t.IsInterface) {
                foreach (var concrete in new [] { typeof (List<>), typeof (HashSet<>), typeof (Dictionary<,>) }) {
                    if (concrete.GetGenericArguments ().Length != a.Length && !(concrete == typeof (Dictionary<,>) && a.Length == 1 && a [0].IsGenericType && a [0].GetGenericTypeDefinition () == typeof (KeyValuePair<,>)))
                        continue;
                    var args = concrete == typeof (Dictionary<,>) && a.Length == 1 ? a [0].GetGenericArguments () : a;
                    Type closed;
                    try { closed = concrete.MakeGenericType (args); } catch (ArgumentException) { continue; }
                    if (t.IsAssignableFrom (closed))
                        return Value (closed, depth + 1);
                }
                return null;
            }
        }
        if (t == typeof (IEnumerator))
            return Started (((IEnumerable) Value (typeof (List<string>), depth + 1)).GetEnumerator ());
        if (t.IsInterface) {
            var list = typeof (List<string>);
            return t.IsAssignableFrom (list) ? Value (list, depth + 1) : null;
        }
        if (t.IsValueType && t.Name == "Enumerator" && t.DeclaringType != null)
            return StructEnumerator (t, depth);
        if (t.IsAbstract)
            return null;
        return Construct (t, depth);
    }

    // An enumerator moved onto its first element, so `Current` is one.
    static object Started (object enumerator)
    {
        if (enumerator == null)
            return null;
        try {
            enumerator.GetType ().GetMethod ("MoveNext", Type.EmptyTypes)?.Invoke (enumerator, null);
        } catch (TargetInvocationException) { }
        return enumerator;
    }

    // A collection's own struct enumerator: built by the collection's `GetEnumerator`.
    object StructEnumerator (Type t, int depth)
    {
        var outer = t.DeclaringType;
        if (outer.IsGenericTypeDefinition)
            outer = outer.MakeGenericType (t.GetGenericArguments ());
        var owner = Value (outer, depth + 1);
        var get = outer.GetMethods ().FirstOrDefault (m => m.Name == "GetEnumerator" && m.ReturnType == t && m.GetParameters ().Length == 0);
        if (owner == null || get == null)
            return null;
        return Started (get.Invoke (owner, null));
    }

    // A concrete type: through the constructor that takes the most it can carry markers in, then,
    // for an empty collection, two elements.
    object Construct (Type t, int depth)
    {
        var ctors = t.GetConstructors ()
            .Where (c => c.GetParameters ().All (p => !p.ParameterType.IsPointer && !p.ParameterType.IsByRefLike && !p.ParameterType.IsByRef))
            .OrderByDescending (c => c.GetParameters ().Count (p => Carries (p.ParameterType)))
            .ThenBy (c => c.GetParameters ().Length)
            .ToList ();
        object obj = null;
        foreach (var c in ctors) {
            try {
                obj = c.Invoke (c.GetParameters ().Select (p => Value (p.ParameterType, depth + 1)).ToArray ());
                break;
            } catch (Exception) { }
        }
        if (obj == null && t.IsValueType)
            obj = Activator.CreateInstance (t);
        if (obj == null)
            return null;
        Fill (obj, t, depth);
        return obj;
    }

    static bool Carries (Type t) => t == typeof (string) || t == typeof (object) || t.IsArray
        || (typeof (IEnumerable).IsAssignableFrom (t) && t != typeof (string)) || typeof (Stream).IsAssignableFrom (t)
        || typeof (TextReader).IsAssignableFrom (t);

    void Fill (object obj, Type t, int depth)
    {
        if (obj is not IEnumerable en || obj is string)
            return;
        try {
            if (en.GetEnumerator ().MoveNext ())
                return;
        } catch (Exception) {
            return;
        }
        int count = rng.Next (1, 4);
        foreach (var name in new [] { "Add", "Enqueue", "Push", "AddLast", "TryAdd" }) {
            var adds = t.GetMethods ().Where (m => m.Name == name && !m.IsStatic && !m.IsGenericMethodDefinition
                && m.GetParameters ().Length is 1 or 2 && m.GetParameters ().All (p => !p.ParameterType.IsByRef && p.ParameterType != typeof (int)))
                .OrderByDescending (m => m.GetParameters ().Count (p => p.ParameterType == typeof (string)))
                .ToList ();
            foreach (var add in adds) {
                for (int i = 0; i < count; i++) {
                    try {
                        add.Invoke (obj, add.GetParameters ().Select (p => Value (p.ParameterType, depth + 1)).ToArray ());
                    } catch (Exception) { }
                }
                try {
                    if (en.GetEnumerator ().MoveNext ())
                        return;
                } catch (Exception) {
                    return;
                }
            }
        }
    }

    // A delegate that passes its first argument through where the types allow, and otherwise
    // returns a constant: `x => x` for a selector, `x => true` for a predicate.
    static Delegate Delegate (Type t)
    {
        var invoke = t.GetMethod ("Invoke");
        if (invoke == null)
            return null;
        var ps = invoke.GetParameters ().Select (p => Expression.Parameter (p.ParameterType.IsByRef ? p.ParameterType.GetElementType () : p.ParameterType)).ToArray ();
        if (invoke.GetParameters ().Any (p => p.ParameterType.IsByRef || p.ParameterType.IsByRefLike))
            return null;
        var ret = invoke.ReturnType;
        Expression body;
        if (ret == typeof (void))
            body = Expression.Empty ();
        else if (ps.Length > 0 && ret.IsAssignableFrom (ps [0].Type))
            body = Expression.Convert (ps [0], ret);
        else if (ret == typeof (bool))
            body = Expression.Constant (true);
        else
            body = Expression.Default (ret);
        try {
            return Expression.Lambda (t, body, ps).Compile ();
        } catch (Exception) {
            return null;
        }
    }
}

static class Runner
{
    sealed class Call
    {
        public object [] Args;      // by port: the receiver (or constructed object) first, if any
        public object Result;
        public string Threw;
    }

    public static string Run (string fid, MethodBase open, int trials, int seed)
    {
        var row = new Dictionary<string, object> { ["fid"] = fid, ["trials"] = trials };
        if (open == null) {
            row ["skip"] = "no such method";
            return JsonSerializer.Serialize (row);
        }
        var method = Close (open, out string why);
        if (method == null) {
            row ["skip"] = why;
            return JsonSerializer.Serialize (row);
        }
        var ps = method.GetParameters ();
        bool ctor = method is ConstructorInfo, instance = !method.IsStatic && !ctor;
        int shift = instance || ctor ? 1 : 0;
        var types = new Type [ps.Length + shift];
        if (shift == 1)
            types [0] = method.DeclaringType;
        for (int i = 0; i < ps.Length; i++)
            types [i + shift] = ps [i].ParameterType;
        // The prefix of what each port's walk sees: a `ref`/`out` parameter's pointee, and a struct
        // receiver, which the frontend passes as a pointer to it.
        var prefix = new string [types.Length];
        for (int k = 0; k < types.Length; k++)
            prefix [k] = types [k].IsByRef || (k == 0 && shift == 1 && types [0].IsValueType) ? "deref" : "";
        bool returns = method is MethodInfo mi && mi.ReturnType != typeof (void);

        var flows = new SortedDictionary<string, (SortedSet<string> From, Loc To)> (StringComparer.Ordinal);
        var depends = new SortedSet<(int, int)> ();
        var threw = new SortedDictionary<string, int> (StringComparer.Ordinal);
        int ran = 0;
        int fidHash = Stable (fid);
        for (int trial = 0; trial < trials; trial++) {
            var baseline = Invoke (method, types, ctor, instance, seed, fidHash, trial, -1);
            if (baseline.Threw != null) {
                threw [baseline.Threw] = threw.GetValueOrDefault (baseline.Threw) + 1;
                continue;
            }
            ran++;
            var before = Labels (Invoke (method, types, ctor, instance, seed, fidHash, trial, -1, call: false), prefix, ctor);
            if (Environment.GetEnvironmentVariable ("MODEL_RUN_DEBUG") != null) {
                Console.Error.WriteLine ($"{fid} trial {trial}");
                foreach (var (port, text) in Texts (baseline, prefix, returns))
                    Console.Error.WriteLine ($"  after {port}: {text}");
                foreach (var (marker, locs) in before)
                    Console.Error.WriteLine ($"  before zq{marker}x: {string.Join (", ", locs.Select (Show))}");
            }
            foreach (var (to, marker) in Outputs (baseline, prefix, returns)) {
                if (!before.TryGetValue (marker, out var from))
                    continue;
                if (from.Contains (to))
                    continue;
                var key = string.Join ("|", from.Select (Show)) + ">" + Show (to);
                if (!flows.ContainsKey (key))
                    flows [key] = (new SortedSet<string> (from.Select (Show), StringComparer.Ordinal), to);
            }
            // Dependence: rerun with one input redrawn. An output that differs between two runs of
            // the same inputs says nothing, so it is checked against a rerun of the baseline first.
            var again = Invoke (method, types, ctor, instance, seed, fidHash, trial, -1);
            var stable = Texts (baseline, prefix, returns);
            var noisy = Texts (again, prefix, returns);
            for (int i = ctor ? 1 : 0; i < types.Length; i++) {
                var variant = Invoke (method, types, ctor, instance, seed, fidHash, trial, i);
                if (variant.Threw != null)
                    continue;
                var texts = Texts (variant, prefix, returns);
                foreach (var (port, text) in texts) {
                    if (port == i || !stable.TryGetValue (port, out var b) || noisy.GetValueOrDefault (port) != b)
                        continue;
                    if (text != b)
                        depends.Add ((i, port));
                }
            }
        }
        row ["ran"] = ran;
        row ["threw"] = threw;
        row ["flows"] = flows.Values.Select (f => new Dictionary<string, object> {
            ["from"] = f.From.Select (Parse).Select (l => new object [] { l.Port, Segments (l.Path) }).ToList (),
            ["to"] = new object [] { f.To.Port, Segments (f.To.Path) },
        }).ToList ();
        row ["depends"] = depends.Select (d => new [] { d.Item1, d.Item2 }).ToList ();
        return JsonSerializer.Serialize (row);
    }

    static string [] Segments (string path) => path.Length == 0 ? Array.Empty<string> () : path.Split (Walker.Sep);

    static string Show (Loc l) => l.Port.ToString (CultureInfo.InvariantCulture) + ":" + l.Path;

    static Loc Parse (string s)
    {
        int i = s.IndexOf (':');
        return new Loc (int.Parse (s.Substring (0, i), CultureInfo.InvariantCulture), s.Substring (i + 1));
    }

    static int Stable (string s)
    {
        unchecked {
            int h = 17;
            foreach (char c in s)
                h = h * 31 + c;
            return h & 0x7fffffff;
        }
    }

    // Instantiates a generic type and method with `string` where the constraints allow it, else
    // `object`, else `int`.
    static MethodBase Close (MethodBase open, out string why)
    {
        why = null;
        var type = open.DeclaringType;
        if (type.IsGenericTypeDefinition) {
            var args = Instantiate (type.GetGenericArguments ());
            if (args == null) {
                why = "no instantiation of the type's generic parameters";
                return null;
            }
            try {
                type = type.MakeGenericType (args);
            } catch (ArgumentException) {
                why = "no instantiation of the type's generic parameters";
                return null;
            }
        }
        var method = MethodBase.GetMethodFromHandle (open.MethodHandle, type.TypeHandle);
        if (method is MethodInfo mi && mi.IsGenericMethodDefinition) {
            var args = Instantiate (mi.GetGenericArguments ());
            try {
                method = args == null ? null : mi.MakeGenericMethod (args);
            } catch (ArgumentException) {
                method = null;
            }
            if (method == null) {
                why = "no instantiation of the method's generic parameters";
                return null;
            }
        }
        foreach (var p in method.GetParameters ()) {
            var t = p.ParameterType.IsByRef ? p.ParameterType.GetElementType () : p.ParameterType;
            if (t.IsPointer || t.IsFunctionPointer)
                why = "a pointer parameter";
            else if (t.IsByRefLike)
                why = "a ref struct parameter (" + t.Name + ")";
        }
        if (method is MethodInfo r && (r.ReturnType.IsByRef || r.ReturnType.IsByRefLike || r.ReturnType.IsPointer))
            why = "a ref, ref struct or pointer return";
        if (method.DeclaringType.IsByRefLike)
            why = "a ref struct receiver";
        if ((method.CallingConvention & CallingConventions.VarArgs) != 0)
            why = "varargs";
        return why == null ? method : null;
    }

    static Type [] Instantiate (Type [] parameters)
    {
        var result = new Type [parameters.Length];
        for (int i = 0; i < parameters.Length; i++) {
            var p = parameters [i];
            var attrs = p.GenericParameterAttributes;
            result [i] = new [] { typeof (string), typeof (object), typeof (int) }.FirstOrDefault (c =>
                (!attrs.HasFlag (GenericParameterAttributes.ReferenceTypeConstraint) || !c.IsValueType)
                && (!attrs.HasFlag (GenericParameterAttributes.NotNullableValueTypeConstraint) || c.IsValueType)
                && (!attrs.HasFlag (GenericParameterAttributes.DefaultConstructorConstraint) || c.IsValueType || c.GetConstructor (Type.EmptyTypes) != null)
                && p.GetGenericParameterConstraints ().All (k => k.ContainsGenericParameters || k.IsAssignableFrom (c)));
            if (result [i] == null)
                return null;
        }
        return result;
    }

    // One call. `variant` names the one input redrawn, or -1. With `call: false`, the arguments
    // are only built, which is how the labels of the inputs a call was given are read.
    static Call Invoke (MethodBase method, Type [] types, bool ctor, bool instance, int seed, int fid, int trial, int variant, bool call = true)
    {
        var args = new object [types.Length];
        List<string> pool = null;
        for (int k = ctor ? 1 : 0; k < types.Length; k++) {
            int v = k == variant ? 1 : 0;
            var rng = new Random (unchecked (seed * 1000003 + fid * 8191 + trial * 131 + k * 17 + v));
            var gen = new Gen (rng, ((trial * 16 + k) * 2 + v) * 1000, k == 0 ? null : pool);
            try {
                args [k] = gen.Value (types [k]);
            } catch (Exception) {
                args [k] = null;
            }
            if (k == 0 && instance)
                pool = StringsIn (args [0]);
        }
        var result = new Call { Args = args };
        if (!call)
            return result;
        if (instance && args [0] == null) {
            result.Threw = "no receiver";
            return result;
        }
        int shift = instance || ctor ? 1 : 0;
        var actual = args.Skip (shift).ToArray ();
        try {
            if (ctor)
                args [0] = ((ConstructorInfo) method).Invoke (actual);
            else
                result.Result = method.Invoke (instance ? args [0] : null, actual);
        } catch (TargetInvocationException e) {
            result.Threw = (e.InnerException ?? e).GetType ().Name;
            return result;
        } catch (Exception e) {
            result.Threw = e.GetType ().Name;
            return result;
        }
        Array.Copy (actual, 0, args, shift, actual.Length);
        return result;
    }

    // The strings a receiver holds, so an argument can name one: a key it has, an element in it.
    static List<string> StringsIn (object o)
    {
        var found = new List<string> ();
        if (o is IEnumerable en && o is not string) {
            try {
                foreach (var e in en.Cast<object> ().Take (8)) {
                    if (e is string s)
                        found.Add (s);
                    else if (e != null && e.GetType ().IsGenericType && e.GetType ().GetGenericTypeDefinition () == typeof (KeyValuePair<,>))
                        found.AddRange (new [] { "Key", "Value" }.Select (p => e.GetType ().GetProperty (p).GetValue (e)).OfType<string> ());
                }
            } catch (Exception) { }
        } else if (o is string s) {
            found.Add (s);
        }
        return found;
    }

    // Where each marker was in the inputs of a call, before it was made.
    static Dictionary<int, HashSet<Loc>> Labels (Call c, string [] prefix, bool ctor)
    {
        var labels = new Dictionary<int, HashSet<Loc>> ();
        for (int k = ctor ? 1 : 0; k < c.Args.Length; k++) {
            var w = new Walker ();
            w.Visit (c.Args [k], prefix [k]);
            foreach (var (path, marker) in w.Hits) {
                if (!labels.TryGetValue (marker, out var set))
                    labels [marker] = set = new HashSet<Loc> ();
                set.Add (new Loc (k, path));
            }
        }
        return labels;
    }

    static IEnumerable<(Loc, int)> Outputs (Call c, string [] prefix, bool returns)
    {
        if (returns) {
            var w = new Walker ();
            w.Visit (c.Result, "");
            foreach (var (path, marker) in w.Hits)
                yield return (new Loc (-1, path), marker);
        }
        for (int k = 0; k < c.Args.Length; k++) {
            var w = new Walker ();
            w.Visit (c.Args [k], prefix [k]);
            foreach (var (path, marker) in w.Hits)
                yield return (new Loc (k, path), marker);
        }
    }

    static Dictionary<int, string> Texts (Call c, string [] prefix, bool returns)
    {
        var texts = new Dictionary<int, string> ();
        if (c.Threw != null)
            return texts;
        if (returns) {
            var w = new Walker ();
            w.Visit (c.Result, "");
            texts [-1] = w.Text.ToString ();
        }
        for (int k = 0; k < c.Args.Length; k++) {
            var w = new Walker ();
            w.Visit (c.Args [k], prefix [k]);
            texts [k] = w.Text.ToString ();
        }
        return texts;
    }
}
