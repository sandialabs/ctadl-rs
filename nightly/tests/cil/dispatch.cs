// The hierarchy, virtual calls and CHA. Interface
// dispatch with two implementations, a virtual override, and a non-virtual
// `callvirt` lowered as a direct call.
namespace Cases
{
    public static class Taint
    {
        public static string Source() => "secret";
        public static void Sink1(object o) { }
        public static void Sink2(object o) { }
        public static void Sink3(object o) { }
        public static void Sink4(object o) { }
    }

    public interface IProvider
    {
        string Get();
    }

    public class Tainted : IProvider
    {
        public string Get() => Taint.Source();
    }

    public class Clean : IProvider
    {
        public string Get() => "clean";
    }

    public class Base
    {
        public virtual string Make() => "base";

        public string NonVirtual(string s) => s;
    }

    public class Derived : Base
    {
        public override string Make() => Taint.Source();
    }

    public sealed class Final
    {
        public string Pass(string s) => s;
    }

    public static class Dispatch
    {
        public static void Interface(bool c)
        {
            IProvider p = c ? new Tainted() : new Clean();
            Taint.Sink1(p.Get());
        }

        public static void Virtual()
        {
            Base b = new Derived();
            Taint.Sink2(b.Make());
        }

        // C# calls a non-virtual instance method with `callvirt`, for the null check.
        public static void NonVirtual() => Taint.Sink3(new Base().NonVirtual(Taint.Source()));

        public static void Sealed() => Taint.Sink4(new Final().Pass(Taint.Source()));
    }
}
