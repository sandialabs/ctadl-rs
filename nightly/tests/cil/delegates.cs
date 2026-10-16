// Slice 9 (translation §11): delegates and lambdas. A lambda, a closure over a
// local, a static method group (bound through a thunk, `calls.rs`), a handler
// held in a field, and a lambda that writes the local it captured.
using System;

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
        public static void Clean1(object o) { }
    }

    public class Button
    {
        public Action<string> OnClick;

        public void Click(string s) => OnClick(s);
    }

    public static class Delegates
    {
        static string Id(string s) => s;

        static string Constant(string s) => "clean";

        public static void Lambda()
        {
            Func<string, string> f = s => s;
            Taint.Sink1(f(Taint.Source()));
        }

        public static void Closure()
        {
            string x = Taint.Source();
            Func<string> g = () => x;
            Taint.Sink2(g());
        }

        public static void MethodGroup()
        {
            Func<string, string> h = Id;
            Taint.Sink3(h(Taint.Source()));
        }

        public static void Handler()
        {
            var b = new Button();
            b.OnClick = s => Taint.Sink4(s);
            b.Click(Taint.Source());
        }

        // The lambda writes the display class's field, which `x` is.
        public static void CapturedWrite()
        {
            string x = "clean";
            Action set = () => x = Taint.Source();
            set();
            Taint.Sink5(x);
        }

        public static void Clean()
        {
            Func<string, string> c = Constant;
            Taint.Clean1(c(Taint.Source()));
        }
    }
}
