// Direct calls and extern stubs. Static, instance and
// constructor calls; a callee in the BCL, which the frontend stubs.
namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Sink3(object o) { }
        public static void Sink4(object o) { }
        public static void Clean1(object o) { }
    }

    public class Holder
    {
        string value;

        public Holder(string v) { value = v; }

        public string Get() => value;

        public void Set(string v) { value = v; }
    }

    public static class Calls
    {
        static string Helper(string s) => Inner(s);

        static string Inner(string s) => s;

        public static void Chain() => Taint.Sink1(Helper(Taint.Source()));

        public static void Constructor() => Taint.Sink2(new Holder(Taint.Source()).Get());

        public static void Setter()
        {
            var h = new Holder("clean");
            h.Set(Taint.Source());
            Taint.Sink3(h.Get());
        }

        // A call into the BCL has no body and no model: the call does not crash the
        // import, and the argument does not flow through it.
        public static void Extern()
        {
            string x = Taint.Source();
            Taint.Clean1(string.IsNullOrEmpty(x));
            Taint.Sink4(x);
        }
    }
}
