// Generic overrides and explicit implementations.
// CIL has no bridge methods, so each needs an override alias (`hierarchy.rs`) for CHA to
// find it under the key the call site uses.
using System.Collections.Generic;

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

    public class Store<T>
    {
        public virtual T Echo(T x) => default;
    }

    // `Echo(string)` overrides `Store<T>.Echo(!0)`: the descriptors differ.
    public class StringStore : Store<string>
    {
        public override string Echo(string x) => x;
    }

    public interface IFoo
    {
        string Bar(string s);
    }

    // An explicit implementation is named `Cases.IFoo.Bar`, not `Bar`.
    public class Explicit : IFoo
    {
        string IFoo.Bar(string s) => s;
    }

    public interface IBox<T>
    {
        T Put(T x);
    }

    // An implicit implementation of a generic interface: `Put(string)` against `Put(!0)`.
    public class StringBox : IBox<string>
    {
        public string Put(string x) => x;
    }

    // A BCL interface whose metadata comes from the framework.
    public class FirstWins : IComparer<string>
    {
        public string Last;

        public int Compare(string a, string b)
        {
            Last = a;
            return 0;
        }
    }

    public static class Overrides
    {
        public static void Generic()
        {
            Store<string> s = new StringStore();
            Taint.Sink1(s.Echo(Taint.Source()));
        }

        public static void ExplicitImpl()
        {
            IFoo f = new Explicit();
            Taint.Sink2(f.Bar(Taint.Source()));
        }

        public static void GenericInterface()
        {
            IBox<string> b = new StringBox();
            Taint.Sink3(b.Put(Taint.Source()));
        }

        public static void Comparer()
        {
            var c = new FirstWins();
            IComparer<string> i = c;
            i.Compare(Taint.Source(), "clean");
            Taint.Sink4(c.Last);
        }
    }
}
