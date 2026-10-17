// Async methods (plan §9): an `async` method compiles to a state machine struct
// whose `MoveNext` holds the whole body. The method itself only builds the struct
// and hands it to its builder's `Start`, which runs `MoveNext` inside the library;
// the result travels through the builder's task.
using System.Threading.Tasks;

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

    public static class Async
    {
        static async Task<string> ProduceAsync()
        {
            await Task.Yield();
            return Taint.Source();
        }

        static async Task<string> EchoAsync(string s)
        {
            await Task.Yield();
            return s;
        }

        static async Task LeakAsync(string s)
        {
            await Task.Yield();
            Taint.Sink3(s);
        }

        static async Task<string> ConstantAsync()
        {
            await Task.Yield();
            return "clean";
        }

        // The result comes from the async method's body, awaited by an async caller.
        public static async Task Produced()
        {
            var x = await ProduceAsync();
            Taint.Sink1(x);
        }

        // The result comes from the argument, read synchronously from the task.
        public static void Echoed()
        {
            var x = EchoAsync(Taint.Source()).Result;
            Taint.Sink2(x);
        }

        // The sink is in the async method's body, after an await.
        public static async Task Leaked()
        {
            await LeakAsync(Taint.Source());
        }

        // Through GetAwaiter().GetResult(), as sync-over-async code does.
        public static void Awaited()
        {
            var x = EchoAsync(Taint.Source()).GetAwaiter().GetResult();
            Taint.Sink4(x);
        }

        public static async Task Clean()
        {
            var x = await ConstantAsync();
            Taint.Sink5(x);
        }
    }
}
