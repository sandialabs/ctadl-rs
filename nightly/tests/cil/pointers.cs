// Managed pointers. `out` and `ref` parameters
// (copy-in/copy-out), a struct's fields through `ldloca`, a struct method
// writing `this`, `a[i] op= x` through `ldelema`, and a `T*` field.
namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static int SourceInt() => 7;
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Sink3(object o) { }
        public static void Sink4(object o) { }
        public static void Sink5(object o) { }
        public static void Sink6(object o) { }
        public static void Sink7(object o) { }
        public static void Clean1(object o) { }
    }

    public struct Pair
    {
        public string First;
        public string Second;

        public void SetFirst(string s) { First = s; }
    }

    public unsafe struct Cell
    {
        public int* P;
    }

    public static class Pointers
    {
        static bool TryGet(string key, out string value)
        {
            value = key;
            return true;
        }

        static void Accumulate(ref string acc, string x) => acc = x;

        public static void Out()
        {
            if (TryGet(Taint.Source(), out var v))
                Taint.Sink1(v);
        }

        public static void Ref()
        {
            string acc = "clean";
            Accumulate(ref acc, Taint.Source());
            Taint.Sink2(acc);
        }

        public static void StructField()
        {
            Pair p = default;
            p.First = Taint.Source();
            Taint.Sink3(p.First);
            Taint.Clean1(p.Second);
        }

        public static void StructMethod()
        {
            Pair p = default;
            p.SetFirst(Taint.Source());
            Taint.Sink4(p.First);
        }

        // `ldelema; dup; ldind; add; stind`: the address the layer cannot trace.
        public static void Compound(int[] a, int i)
        {
            a[i * 2] += Taint.SourceInt();
            Taint.Sink5(a[0]);
        }

        public static void RefLocal()
        {
            var ps = new Pair[2];
            ref Pair r = ref ps[1];
            r.Second = Taint.Source();
            Taint.Sink6(ps[0].Second);
        }

        public static unsafe void PointerField()
        {
            int x = Taint.SourceInt();
            Cell c;
            c.P = &x;
            Taint.Sink7(*c.P);
        }
    }
}
