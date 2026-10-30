// The default .NET models (cil-index.ctadl): taint through library calls
// the frontend stubs, at the call shapes C# compiles them to. Each scenario sinks into its own
// function.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json;

namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Sink3(object o) { }
        public static void Sink4(object o) { }
        public static void Sink5(object o) { }
        public static void Sink6(object o) { }
        public static void Sink7(object o) { }
        public static void Sink8(object o) { }
        public static void Sink9(object o) { }
        public static void Sink10(object o) { }
        public static void Sink11(object o) { }
        public static void Sink12(object o) { }
        public static void Sink13(object o) { }
        public static void Sink14(object o) { }
        public static void Clean1(object o) { }
        public static void Clean2(object o) { }
    }

    public static class Library
    {
        // String.Concat, and Format through its `params object[]`.
        public static void Strings()
        {
            string s = Taint.Source();
            string t = "<" + s + ">";
            Taint.Sink1(string.Format("{0} {1} {2} {3}", 1, 2, 3, t.Trim()));
        }

        // An interpolated string: a DefaultInterpolatedStringHandler local, called through its
        // address.
        public static void Interpolation(int n)
        {
            string s = Taint.Source();
            Taint.Sink2($"n={n}, s={s}");
        }

        // A StringBuilder written by statements, and one whose chain of writers ends in ToString.
        // (A chain whose result is dropped, followed by a read of the builder, is not followed: no
        // model can say a writer returns its own receiver.)
        public static void Builder()
        {
            var sb = new StringBuilder();
            sb.Append("a");
            sb.Append(Taint.Source());
            Taint.Sink3(sb.ToString());
            Taint.Sink14(new StringBuilder("a").Append(Taint.Source()).AppendLine().ToString());
        }

        // A list's elements, read back by a `foreach` over its struct enumerator.
        public static void ListForeach()
        {
            var list = new List<string>();
            list.Add(Taint.Source());
            foreach (var x in list)
                Taint.Sink4(x);
        }

        // A dictionary read through TryGetValue's out parameter.
        public static void DictionaryOut()
        {
            var d = new Dictionary<string, string>();
            d["k"] = Taint.Source();
            if (d.TryGetValue("k", out var v))
                Taint.Sink5(v);
        }

        // A dictionary's pairs, through its struct enumerator and KeyValuePair.Value.
        public static void DictionaryForeach()
        {
            var d = new Dictionary<string, string> { ["k"] = Taint.Source() };
            foreach (var kv in d)
                Taint.Sink6(kv.Value);
        }

        // A collection behind an interface: the calls are virtual, on the interface's methods.
        public static void Interface(IList<string> list)
        {
            list.Add(Taint.Source());
            Taint.Sink7(list[0]);
        }

        // LINQ: a query over the elements, and one element out of it.
        public static void Linq()
        {
            var xs = new[] { Taint.Source(), "b" };
            Taint.Sink8(xs.Select(x => x.ToUpper()).Where(x => x.Length > 0).First());
        }

        // Bytes and back: an encoding is abstract, so these calls are virtual.
        public static void Encode()
        {
            byte[] b = Encoding.UTF8.GetBytes(Taint.Source());
            Taint.Sink9(Convert.ToBase64String(b));
        }

        // A parsed number formatted again: int.ToString is called on the struct's address.
        public static void Number()
        {
            int n = int.Parse(Taint.Source());
            Taint.Sink10(n.ToString());
        }

        // A tuple's fields, written by its constructor and read by name.
        public static void Tuple()
        {
            var t = (Taint.Source(), 1);
            Taint.Sink11(t.Item1);
        }

        // JSON in and out.
        public static void Json()
        {
            var doc = JsonDocument.Parse(Taint.Source());
            Taint.Sink12(doc.RootElement.GetProperty("k").GetString());
        }

        // Object.ToString on something whose class the program does not see.
        public static void ToStringVirtual()
        {
            object o = new Uri(Taint.Source());
            Taint.Sink13(o.ToString());
        }

        // A predicate is not a propagation: no model, so no flow.
        public static void Predicate()
        {
            Taint.Clean1(string.IsNullOrEmpty(Taint.Source()));
        }

        // Taint in one dictionary does not reach another.
        public static void OtherDictionary()
        {
            var tainted = new Dictionary<string, string> { ["k"] = Taint.Source() };
            var clean = new Dictionary<string, string> { ["k"] = "v" };
            Taint.Clean2(clean["k"]);
        }
    }
}
