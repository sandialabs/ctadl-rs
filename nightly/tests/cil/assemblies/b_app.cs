// Multiple assemblies and forwarding. The app calls
// into a library imported beside it, overrides one of its virtual methods, and
// overrides `ToString` of `System.Object`, which the app references through the
// `System.Runtime` facade and the frontend canonicalizes to
// `System.Private.CoreLib`.
namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Sink3(object o) { }
    }

    public class Fancy : Lib.Widget
    {
        public override string Render(string s) => s;
    }

    public class Shows
    {
        readonly string text;

        public Shows(string t) { text = t; }

        public override string ToString() => text;
    }

    public static class App
    {
        public static void CrossAssembly() => Taint.Sink1(Lib.Util.Pass(Taint.Source()));

        public static void CrossOverride()
        {
            Lib.Widget w = new Fancy();
            Taint.Sink2(w.Render(Taint.Source()));
        }

        public static void Facade()
        {
            object o = new Shows(Taint.Source());
            Taint.Sink3(o.ToString());
        }
    }
}
