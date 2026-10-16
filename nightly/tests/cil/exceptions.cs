// Slice 7 (translation §11): exceptions. A value carried by a thrown object
// reaches the handler, through a call that throws (split-block CFG, §8) and
// out of a method whose catch does not match.
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
    }

    public class DataException : Exception
    {
        public string Data2;

        public DataException(string d) { Data2 = d; }
    }

    public class OtherException : Exception { }

    public static class Exceptions
    {
        static void Thrower(string d) => throw new DataException(d);

        public static void Local()
        {
            try
            {
                throw new DataException(Taint.Source());
            }
            catch (DataException e)
            {
                Taint.Sink1(e.Data2);
            }
        }

        public static void ThroughCall()
        {
            try
            {
                Thrower(Taint.Source());
            }
            catch (DataException e)
            {
                Taint.Sink2(e.Data2);
            }
        }

        // The catch does not match: the exception leaves the method.
        static void Passes(string d)
        {
            try
            {
                Thrower(d);
            }
            catch (OtherException)
            {
            }
        }

        public static void Escapes()
        {
            try
            {
                Passes(Taint.Source());
            }
            catch (DataException e)
            {
                Taint.Sink3(e.Data2);
            }
        }

        // A finally runs and the exception goes on.
        static void WithFinally(string d)
        {
            try
            {
                Thrower(d);
            }
            finally
            {
                Taint.Source();
            }
        }

        public static void ThroughFinally()
        {
            try
            {
                WithFinally(Taint.Source());
            }
            catch (DataException e)
            {
                Taint.Sink4(e.Data2);
            }
        }
    }
}
