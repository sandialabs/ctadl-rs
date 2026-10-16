// Slice 3 (translation §11): fields, statics, arrays.
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
        public static void Clean2(object o) { }
    }

    public class Box
    {
        public string Value;
        public string Other;
        public Box Next;
    }

    public static class Memory
    {
        static string Shared;

        public static void Field()
        {
            var b = new Box();
            b.Value = Taint.Source();
            Taint.Sink1(b.Value);
            Taint.Clean1(b.Other);
        }

        public static void Nested()
        {
            var b = new Box { Next = new Box() };
            b.Next.Value = Taint.Source();
            Taint.Sink2(b.Next.Value);
        }

        public static void Static()
        {
            Shared = Taint.Source();
            Taint.Sink3(Shared);
        }

        public static void Array()
        {
            var a = new string[4];
            a[1] = Taint.Source();
            Taint.Sink4(a[2]);
        }

        // An element of an array of strings stored through an object.
        public static void ArrayInField(int i)
        {
            var b = new Box[] { new Box() };
            b[0].Value = Taint.Source();
            Taint.Sink5(b[i].Value);
        }

        // The length of an array says nothing about its elements' taint.
        public static void Length()
        {
            var a = new string[] { Taint.Source() };
            Taint.Clean2(a.Length);
        }
    }
}
