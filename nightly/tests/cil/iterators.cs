// Iterators (plan §9): a `yield return` method compiles to a state machine class
// whose `MoveNext` holds the whole body, and whose `Current` reads a generated
// field. A `foreach` over it reaches both through the `IEnumerator<T>` interface.
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
        public static void Sink5(object o) { }
    }

    public static class Iterators
    {
        static IEnumerable<string> Produce()
        {
            yield return "clean";
            yield return Taint.Source();
        }

        static IEnumerable<string> Pass(IEnumerable<string> xs)
        {
            foreach (var x in xs)
                yield return x;
        }

        static IEnumerable<int> Leak(string s)
        {
            Taint.Sink3(s);
            yield return 1;
        }

        static IEnumerable<string> Constant()
        {
            yield return "clean";
        }

        // The element comes from the iterator's body.
        public static void Produced()
        {
            foreach (var x in Produce())
                Taint.Sink1(x);
        }

        // The element comes from the iterator's argument, through a list.
        public static void Passed()
        {
            var xs = new List<string> { Taint.Source() };
            foreach (var x in Pass(xs))
                Taint.Sink2(x);
        }

        // The sink is in the iterator's body, which runs only when it is enumerated.
        public static void Leaked()
        {
            foreach (var _ in Leak(Taint.Source())) { }
        }

        // An iterator's parameter is a field of the state machine, held across MoveNext.
        public static void Captured()
        {
            var s = Taint.Source();
            foreach (var x in Pass(new[] { s }))
                Taint.Sink4(x);
        }

        public static void Clean()
        {
            foreach (var x in Constant())
                Taint.Sink5(x);
        }
    }
}
