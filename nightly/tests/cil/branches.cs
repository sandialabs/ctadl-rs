// Slice 2 (translation §11): branches and loops. Values that cross a join on
// the stack (Release keeps `?:` operands there) and loop-carried locals.
namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Sink3(object o) { }
        public static void Clean1(object o) { }
    }

    public static class Branches
    {
        public static string Pick(bool c, string a, string b) => c ? a : b;

        public static void Ternary(bool c) => Taint.Sink1(c ? Taint.Source() : "clean");

        public static void ThroughPick(bool c) => Taint.Sink2(Pick(c, "clean", Taint.Source()));

        public static void Loop(int n)
        {
            string r = "clean";
            string x = Taint.Source();
            for (int i = 0; i < n; i++)
            {
                if (i % 2 == 0)
                    r = x;
            }
            Taint.Sink3(r);
        }

        // A branch on tainted data taints nothing (§9: `Test` emits nothing).
        public static void Implicit()
        {
            string x = Taint.Source();
            string r = x == null ? "a" : "b";
            Taint.Clean1(r);
        }
    }
}
