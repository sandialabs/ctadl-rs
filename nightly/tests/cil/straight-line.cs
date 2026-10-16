// Slice 1 (translation §11): straight-line static methods. Arguments, locals,
// copies, constants and returns.
namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Clean1(object o) { }
        public static void Clean2(object o) { }
    }

    public static class StraightLine
    {
        public static string Id(string s) => s;

        static string Second(string a, string b) => b;

        // Through an identity function.
        public static void Identity() => Taint.Sink1(Id(Taint.Source()));

        // Through locals, and the second of two arguments.
        public static void Locals()
        {
            string x = Taint.Source();
            string y = x;
            Taint.Sink2(Second("clean", y));
        }

        // A constant reaches the sink, not the source.
        public static void Constant()
        {
            string x = Taint.Source();
            Taint.Clean1(Id("clean"));
        }

        // The first argument is dropped.
        public static void Dropped() => Taint.Clean2(Second(Taint.Source(), "clean"));
    }
}
