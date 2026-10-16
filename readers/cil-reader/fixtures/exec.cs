// exec.cs: methods for the `cil:exec` check, which runs each one on .NET with
// random arguments (tools/FlowRun) and again in xtask's interpreter over
// cil-reader's MethodFlow, and compares the results. The flake compiles it
// twice, Release (exec.cs.dll) and Debug (exec-debug.cs.dll), because the two
// shape the IL differently: Release keeps values on the stack across branches
// and joins, Debug spills everything to locals and adds branches to the `ret`.
//
// The interpreter models only what this file uses, so keep to it:
// - no strings and no generics;
// - delegates only of this file's own types (a BCL delegate's type does not
//   resolve without the framework), bound to methods of this module;
// - no BCL calls beyond the Object and Exception constructors (no Math, no
//   array initializers, which call RuntimeHelpers.InitializeArray);
// - no `static readonly` fields, which the harness cannot reset;
// - every loop and recursion bounded whatever the arguments, since the
//   arguments are random.

using System;

namespace Exec
{
    public class Node
    {
        public int Value;
        public Node Next;
    }

    public class Bag
    {
        public int A;
        public long B;
        public double C;
        public float F;
        public byte U8;
        public sbyte S8;
        public short S16;
        public ushort U16;
        public char Ch;
        public bool Flag;
        public uint U32;
        public ulong U64;
        public int[] Arr;
        public Pt P;
        public object O;
    }

    public struct Pt
    {
        public int X;
        public int Y;

        public Pt(int x, int y)
        {
            X = x;
            Y = y;
        }

        public int Sum() => X + Y;

        public void Scale(int k)
        {
            X *= k;
            Y *= k;
        }
    }

    public struct Rect
    {
        public Pt Lo;
        public Pt Hi;
        public byte Tag;
    }

    public class Shape
    {
        public int Id;

        public virtual int Area() => Id;

        public virtual int Kind() => 1;

        public int Describe() => Area() * 10 + Kind();
    }

    public class Sq : Shape
    {
        public int S;

        public override int Area() => S * S;
    }

    public class Rc : Sq
    {
        public int W;

        public override int Area() => S * W;

        public override int Kind() => base.Kind() + 2;
    }

    public class Hider : Rc
    {
        // A new slot: a call through Shape still reaches Rc.Area.
        public new virtual int Area() => -1;
    }

    public class MyEx : Exception
    {
        public int Code;

        public MyEx(int code)
        {
            Code = code;
        }
    }

    public class OtherEx : MyEx
    {
        public OtherEx(int code) : base(code + 1) { }
    }

    public static class G
    {
        public static int Counter;
        public static long Total;
        public static double D;
        public static int[] Data;
        public static Pt P;
        public static Node Head;
        public static object O;
    }

    public static class Arith
    {
        public static int AddMul(int a, int b, int c) => a + b * c;

        public static int DivRem(int a, int b) => a / b + a % b;

        public static uint UDivRem(uint a, uint b) => a / b + a % b;

        public static long LDivRem(long a, long b) => a / b - a % b;

        public static ulong ULDivRem(ulong a, ulong b) => a / b ^ a % b;

        public static long Mixed(int a, long b) => a * b - (b >> 3) + (a << 2);

        public static ulong ULong(ulong a, ulong b) => (a >> 7) ^ (b * 3) | (a << 61);

        public static int Shifts(int a, int s) => (a << s) | (a >> s) ^ (int)((uint)a >> s);

        public static long LShifts(long a, int s) => (a << s) + (a >> s) + (long)((ulong)a >> s);

        public static int Checked(int a, int b) => checked(a * b + a);

        public static long CheckedL(long a, long b) => checked(a - b * 2);

        public static uint CheckedU(uint a, uint b) => checked(a - b);

        public static ulong CheckedUL(ulong a, ulong b) => checked(a * b + 1);

        public static int Convs(long x) => (sbyte)x + (byte)x + (short)x + (ushort)x + (int)(uint)x;

        public static long Widen(int a, uint b, short c, byte d, char e, sbyte f, ushort g) =>
            a + b + c + d + e + f + g;

        public static int CheckedConv(long x) => checked((int)x) + checked((byte)(x & 0x1ff));

        public static uint CheckedToUnsigned(int x) => checked((uint)x) + checked((ushort)(x >> 4));

        public static sbyte CheckedNarrow(int x) => checked((sbyte)x);

        public static double Dbl(double a, double b) => a * b + a / b - b % a;

        public static float Flt(float a, float b) => a * b + a / b - b % a;

        public static double MixedFloat(float a, double b, int c, long d) => a + b * c - d / 3.0;

        public static int Cmp(int a, int b) =>
            (a < b ? 1 : 0) + (a > b ? 2 : 0) + (a == b ? 4 : 0) + ((uint)a < (uint)b ? 8 : 0)
            + ((uint)a >= (uint)b ? 16 : 0) + (a <= b ? 32 : 0);

        public static int LCmp(long a, ulong b) =>
            (a < 0 ? 1 : 0) | ((ulong)a > b ? 2 : 0) | (a == (long)b ? 4 : 0);

        public static int FCmp(double a, double b) =>
            (a < b ? 1 : 0) | (a > b ? 2 : 0) | (a == b ? 4 : 0) | (a != b ? 8 : 0)
            | (!(a < b) ? 16 : 0) | (!(a >= b) ? 32 : 0) | (a <= b ? 64 : 0);

        public static int FloatToInt(double d) => d > -1e9 && d < 1e9 ? (int)d : -7;

        public static long FloatToLong(float f) => f > -1e15f && f < 1e15f ? (long)f : 3;

        public static uint FloatToUInt(double d) => d > -0.5 && d < 4e9 ? (uint)d : 9;

        public static int CheckedFloat(double d)
        {
            try
            {
                return checked((int)d);
            }
            catch (OverflowException)
            {
                return -11;
            }
        }

        public static double IntToFloat(long a, ulong b, uint c) => (double)a + (double)b + (float)a + c;

        public static float ULongToFloat(ulong a) => a;

        public static bool Logic(bool a, bool b, int c) => a && !b || c > 3 && a ^ b;

        public static int Neg(int a, long b) => -a + (int)~b;

        public static char NextChar(char c) => (char)(c + 1);

        public static byte AddBytes(byte a, byte b) => (byte)(a + b);

        public static short MulShorts(short a, short b) => (short)(a * b);

        public static int Bits(int a, int b) => (a & b) ^ (a | ~b);

        public static double Precision(float a, float b)
        {
            float x = a * b;
            double y = (double)a * b;
            return y - x;
        }
    }

    public static class Ctl
    {
        public static int Loop(int n)
        {
            int s = 0;
            for (int i = 0; i < (n & 31); i++)
                s += i * i;
            return s;
        }

        public static int Collatz(int n)
        {
            n &= 0xffff;
            int steps = 0;
            while (n != 1 && n != 0 && steps < 200)
            {
                n = (n % 2 == 0) ? n / 2 : 3 * n + 1;
                steps++;
            }
            return steps;
        }

        public static int Switch(int k)
        {
            switch (k)
            {
                case 0: return 10;
                case 1: return 11;
                case 2:
                case 3: return 23;
                case 5: return 55;
                case 6: return k * 7;
                default: return -1;
            }
        }

        public static int SwitchFall(int k, int v)
        {
            int r;
            switch (k & 7)
            {
                case 0: r = v; break;
                case 1: r = v * 2; break;
                case 2: r = -v; break;
                case 4: r = v ^ 5; break;
                case 5: goto case 1;
                default: r = 99; break;
            }
            return r + k;
        }

        public static long SparseSwitch(long k)
        {
            switch (k)
            {
                case -100: return 1;
                case 7: return 2;
                case 1000: return 3;
                case 1001: return 4;
                case 1002: return 5;
                case long.MaxValue: return 6;
                default: return 0;
            }
        }

        public static int Ternary(int a, int b, int c) => Pick(a > b ? a : b, c > 0 ? c : -c, a == c ? 1 : 2);

        public static int Pick(int x, int y, int z) => x * 100 + y * 10 + z;

        public static int NestedTernary(int a, int b) =>
            a + (b > 0 ? (a > 0 ? 1 : 2) : (a < 0 ? 3 : 4)) * (b + 1);

        public static long StackAcrossJoin(int a, long b) =>
            b * (a > 0 ? b + 1 : a < -5 ? b - 1 : 7) + (a & 1) + Pick(a, a > 3 ? 1 : 0, (int)b);

        public static int Goto(int n)
        {
            int i = 0;
        top:
            i += n & 3;
            if (i < 20 && (n & 3) != 0)
                goto top;
            return i;
        }

        public static int Fib(int n)
        {
            n &= 15;
            return n <= 1 ? n : Fib(n - 1) + Fib(n - 2);
        }

        public static int Gcd(int a, int b)
        {
            a &= 0x7fffffff;
            b &= 0x7fffffff;
            int steps = 0;
            while (b != 0 && steps++ < 100)
            {
                int t = a % b;
                a = b;
                b = t;
            }
            return a;
        }

        public static int ShortCircuit(int a, int b) =>
            (a != 0 && b / a > 2) ? 1 : (b != 0 || a / b > 0 ? 2 : 3);

        public static int Nested(int a, int b)
        {
            int s = 0;
            for (int i = 0; i < (a & 7); i++)
            {
                for (int j = 0; j < (b & 7); j++)
                {
                    if (((i ^ j) & 1) == 0)
                        continue;
                    if (i * j > 12)
                        break;
                    s += i - j;
                }
            }
            return s;
        }

        public static bool BoolReturn(int a) => (a > 3) == ((a & 1) == 0);

        public static void VoidReturn(int a)
        {
            if (a > 0)
                return;
            G.Counter = a;
        }

        public static int Params(byte a, sbyte b, short c, ushort d, char e, bool f, uint g, ulong h, float i, double j) =>
            a + b + c + d + e + (f ? 1 : 0) + (int)g + (int)h + (i > 0 ? 1 : 0) + (j < 0 ? 2 : 0);

        public static int StargArg(int a)
        {
            a += 5;
            a *= 2;
            return a;
        }
    }

    public static class Exc
    {
        public static int CatchDiv(int a, int b)
        {
            try
            {
                return a / b;
            }
            catch (DivideByZeroException)
            {
                return -1;
            }
        }

        public static int Finally(int a)
        {
            int x = 0;
            try
            {
                x = 10 / a;
            }
            finally
            {
                G.Counter++;
            }
            return x;
        }

        public static int ReturnInTry(int a)
        {
            try
            {
                if (a > 0)
                    return a;
                G.Counter += 2;
            }
            finally
            {
                G.Counter += 3;
            }
            return -a;
        }

        public static int NestedFinally(int a, int b)
        {
            int r = 0;
            try
            {
                try
                {
                    r = a / b;
                }
                finally
                {
                    r += 100;
                    G.Counter = r;
                }
            }
            catch (ArithmeticException)
            {
                r -= 7;
            }
            return r;
        }

        public static int Filter(int a)
        {
            try
            {
                throw new MyEx(a);
            }
            catch (MyEx e) when (e.Code > 3)
            {
                return e.Code;
            }
            catch (MyEx e)
            {
                return -e.Code;
            }
        }

        public static int FilterThrows(int a)
        {
            try
            {
                throw new MyEx(a);
            }
            catch (MyEx e) when (10 / e.Code > 2)
            {
                return 1;
            }
            catch (Exception)
            {
                return 2;
            }
        }

        public static int Rethrow(int a)
        {
            try
            {
                try
                {
                    if (a < 0)
                        throw new MyEx(a);
                    return a;
                }
                catch (MyEx)
                {
                    G.Counter++;
                    throw;
                }
            }
            catch (Exception)
            {
                return 77;
            }
        }

        public static int Throws(int a)
        {
            if (a > 5)
                throw new OtherEx(a);
            return a;
        }

        public static int CatchBase(int a)
        {
            try
            {
                return Throws(a);
            }
            catch (MyEx e)
            {
                return e.Code * 2;
            }
        }

        public static int Uncaught(int a) => Throws(a) + 1;

        public static int Nre(Node n)
        {
            try
            {
                return n.Value;
            }
            catch (NullReferenceException)
            {
                return -5;
            }
        }

        public static int Ior(int[] arr, int i)
        {
            try
            {
                return arr[i];
            }
            catch (IndexOutOfRangeException)
            {
                return -6;
            }
            finally
            {
                G.Counter++;
            }
        }

        public static int LoopWithTry(int n)
        {
            int s = 0;
            for (int i = 0; i < (n & 7); i++)
            {
                try
                {
                    s += 10 / (i - 2);
                }
                catch (DivideByZeroException)
                {
                    s += 1000;
                    continue;
                }
                finally
                {
                    s++;
                }
                s *= 2;
            }
            return s;
        }

        public static int BreakThroughFinally(int n)
        {
            int s = 0;
            while (true)
            {
                try
                {
                    s++;
                    if (s > (n & 7))
                        break;
                }
                finally
                {
                    s += 10;
                }
            }
            return s;
        }

        public static int ThrowInFinally(int a)
        {
            try
            {
                try
                {
                    if (a == 0)
                        throw new MyEx(1);
                }
                finally
                {
                    if (a < 0)
                        throw new MyEx(2);
                }
            }
            catch (MyEx e)
            {
                return e.Code;
            }
            return 0;
        }

        public static int ThrowInCatch(int a)
        {
            try
            {
                try
                {
                    return 10 / a;
                }
                catch (DivideByZeroException)
                {
                    throw new MyEx(a + 40);
                }
                finally
                {
                    G.Counter += 5;
                }
            }
            catch (MyEx e)
            {
                return e.Code;
            }
        }

        public static int CatchAll(int a)
        {
            try
            {
                return Throws(a) + 100 / (a - 1);
            }
            catch
            {
                return -3;
            }
        }

        // Filters run in the first pass, before the inner finally.
        public static long TwoPass(int a)
        {
            G.Counter = 0;
            try
            {
                try
                {
                    throw new MyEx(a);
                }
                finally
                {
                    G.Counter = 1;
                }
            }
            catch (MyEx e) when (Record(e.Code))
            {
                return G.Total * 10 + G.Counter;
            }
        }

        static bool Record(int code)
        {
            G.Total = G.Counter + code;
            return code != 3;
        }

        public static int CaughtObject(int a)
        {
            MyEx saved = null;
            try
            {
                Throws(a);
            }
            catch (MyEx e)
            {
                saved = e;
            }
            return saved == null ? 0 : saved.Code;
        }

        public static int Stores(Bag b, int a)
        {
            try
            {
                b.A = a;
                b.B = 100 / a;
                b.C = 1.5;
            }
            catch (DivideByZeroException)
            {
                b.U8 = 7;
            }
            return b.A;
        }

        public static int StaticException(int a)
        {
            try
            {
                G.O = new MyEx(a);
                throw (MyEx)G.O;
            }
            catch (MyEx e) when (e == G.O)
            {
                return 1;
            }
        }
    }

    public static class Obj
    {
        public static int Incr(Bag b, int k)
        {
            b.A += k;
            b.B++;
            b.U8 += (byte)k;
            b.S8 -= 3;
            b.S16 *= 3;
            b.U16 ^= 0xff;
            b.Ch++;
            b.F *= 2;
            b.C -= k;
            b.U32 += (uint)k;
            b.U64 *= 5;
            b.Flag = !b.Flag;
            return b.A;
        }

        public static Node Build(int n)
        {
            Node h = null;
            for (int i = 0; i < (n & 7); i++)
                h = new Node { Value = i * n, Next = h };
            return h;
        }

        public static int SumList(Node n)
        {
            int s = 0;
            int c = 0;
            while (n != null && c++ < 10)
            {
                s += n.Value;
                n = n.Next;
            }
            return s;
        }

        public static Node Reverse(Node n)
        {
            Node prev = null;
            int c = 0;
            while (n != null && c++ < 10)
            {
                Node next = n.Next;
                n.Next = prev;
                prev = n;
                n = next;
            }
            return prev;
        }

        public static int Virt(Shape s) => s.Area() + s.Kind() * 1000 + s.Describe();

        public static int MakeShapes(int a)
        {
            Shape s = (a & 1) == 0 ? new Sq { S = a & 0xff } : (a & 2) == 0 ? new Rc { S = a & 0xff, W = 3 } : new Hider { S = 2, W = a };
            return Virt(s) + (s is Rc ? 1 : 0) + (s is Hider h ? h.Area() : 0);
        }

        public static int Cast(Shape s)
        {
            try
            {
                return ((Rc)s).W;
            }
            catch (InvalidCastException)
            {
                return -1;
            }
        }

        public static int AsCast(Shape s)
        {
            Sq q = s as Sq;
            return q == null ? -2 : q.S;
        }

        public static int Aliasing(Bag a, Bag b)
        {
            a.A = 1;
            b.A = 2;
            return a.A;
        }

        public static void SwapFields(Bag a)
        {
            int t = a.A;
            a.A = (int)a.B;
            a.B = t;
        }

        public static int Statics(int a)
        {
            G.Counter += a;
            G.Total += G.Counter;
            G.D = G.D * 0.5 + a;
            return G.Counter;
        }

        public static int Boxing(int a)
        {
            object o = a;
            object p = (long)a;
            return (o is int i ? i : -1) + (p is int ? 1 : 0) + (int)(long)p;
        }

        public static int Unboxing(object o)
        {
            try
            {
                return (int)o;
            }
            catch (InvalidCastException)
            {
                return -1;
            }
            catch (NullReferenceException)
            {
                return -2;
            }
        }

        public static int BoxStruct(int a)
        {
            object o = new Pt(a, 2);
            Pt p = (Pt)o;
            p.X++;
            return ((Pt)o).X * 100 + p.X;
        }

        public static int ChainNew(int a) => new Node { Value = a, Next = new Node { Value = a + 1 } }.Next.Value;

        public static int Compound(Bag b, int[] arr, int i)
        {
            b.Arr = arr;
            b.Arr[i & 3]++;
            arr[(i + 1) & 3] += b.A;
            return arr.Length;
        }

        public static int ChainedAssign(Bag a, Bag b, int v)
        {
            a.A = b.A = v;
            a.U8 = b.U8 = (byte)(v + 300);
            return a.A + b.U8;
        }

        public static int Instance(Shape s) => s.Describe();

        public static object ObjectField(Bag b, int a)
        {
            b.O = a > 0 ? (object)new Node { Value = a } : b;
            return b.O;
        }

        public static int StructField(Bag b, int a)
        {
            b.P.X += a;
            b.P.Scale(3);
            Pt copy = b.P;
            copy.Y = 1;
            return b.P.Y + copy.Y;
        }
    }

    public static class Val
    {
        public static int Ctor(int x, int y)
        {
            var p = new Pt(x, y);
            return p.Sum();
        }

        public static int CtorAsArgument(int x, int y) => Take(new Pt(x, y), new Pt(y, x)) + Ctl.Pick(x, new Pt(y, 1).Sum(), 3);

        static int Take(Pt a, Pt b) => a.X * 3 + b.Y;

        public static int Scale(int x, int k)
        {
            Pt p = new Pt(x, x + 1);
            p.Scale(k);
            return p.X * 7 + p.Y;
        }

        public static int Copy(int x)
        {
            Pt a = new Pt(x, 2);
            Pt b = a;
            b.X = 100;
            return a.X + b.X;
        }

        public static int NestedStruct(Rect r)
        {
            r.Lo.X += 5;
            r.Hi.Scale(2);
            r.Tag += 200;
            return r.Lo.X + r.Hi.Y + r.Tag;
        }

        public static int RefParam(ref Pt p, int k)
        {
            p.X += k;
            p.Scale(2);
            return p.Y;
        }

        public static void Swap(ref int a, ref int b)
        {
            int t = a;
            a = b;
            b = t;
        }

        public static bool TryDiv(int a, int b, out int q)
        {
            if (b == 0)
            {
                q = 0;
                return false;
            }
            q = a / b;
            return true;
        }

        public static int UseTry(int a, int b) => TryDiv(a, b, out var q) ? q : -1;

        public static int CallSwap(int a, int b)
        {
            Swap(ref a, ref b);
            return a * 1000 + b;
        }

        public static void RefBytes(ref byte b, ref long l, ref double d, ref bool f)
        {
            b += 250;
            l = -l;
            d *= 3;
            f = !f;
        }

        public static int ArrayOfStructs(int n)
        {
            var arr = new Pt[4];
            for (int i = 0; i < arr.Length; i++)
                arr[i].X = i * n;
            arr[2].Scale(3);
            arr[3] = new Pt(n, n + 1);
            return arr[2].X + arr[3].Y;
        }

        public static int StaticStruct(int a)
        {
            G.P.X += a;
            G.P.Scale(2);
            return G.P.Y;
        }

        public static int Default(int a)
        {
            Pt p = default;
            p.Y = a;
            Pt q = p;
            p = default;
            return q.Y + p.Y;
        }

        public static int ElemRef(int[] arr, int i)
        {
            ref int r = ref arr[i & 1];
            r += 5;
            return r;
        }

        public static int RefLocal(int a)
        {
            int x = a, y = -a;
            ref int r = ref (a > 0 ? ref x : ref y);
            r *= 3;
            return x * 10 + y;
        }

        // A join of an address that resolves (&x) with one that does not
        // (the ref parameter): the address after it resolves to neither.
        public static int RefJoin(int a, ref int p)
        {
            int x = a * 3;
            ref int r = ref (a > 0 ? ref x : ref p);
            r += 7;
            return x * 100 + p;
        }

        public static int Multi(int a, int b)
        {
            var m = new int[3, 4];
            for (int i = 0; i < 3; i++)
                for (int j = 0; j < 4; j++)
                    m[i, j] = i * a + j * b;
            m[1, 2] += 7;
            ref int r = ref m[2, 3];
            r ^= 0x55;
            return m[1, 2] + m[2, 3];
        }

        public static int Jagged(int n)
        {
            int[][] j = new int[3][];
            j[1] = new int[(n & 3) + 1];
            j[1][n & 3] = n;
            return j[1][(n & 3)] + j[1].Length + (j[0] == null ? 1 : 0);
        }

        public static long PtrToStatic(int a)
        {
            Inc(ref G.Total, a);
            Inc(ref G.Counter, a);
            return G.Total;
        }

        static void Inc(ref long x, int a) => x += a;

        static void Inc(ref int x, int a) => x += a;
    }

    public static class Arr
    {
        public static int Sum(int[] a)
        {
            int s = 0;
            foreach (var x in a)
                s += x;
            return s;
        }

        public static void Reverse(int[] a)
        {
            for (int i = 0, j = a.Length - 1; i < j; i++, j--)
            {
                int t = a[i];
                a[i] = a[j];
                a[j] = t;
            }
        }

        public static void Sort(long[] a)
        {
            for (int i = 0; i < a.Length; i++)
                for (int j = 0; j + 1 < a.Length - i; j++)
                    if (a[j] > a[j + 1])
                    {
                        long t = a[j];
                        a[j] = a[j + 1];
                        a[j + 1] = t;
                    }
        }

        public static double Avg(double[] a)
        {
            double s = 0;
            for (int i = 0; i < a.Length; i++)
                s += a[i];
            return a.Length == 0 ? 0 : s / a.Length;
        }

        public static int[] Make(int n)
        {
            var a = new int[n & 7];
            for (int i = 0; i < a.Length; i++)
                a[i] = i * n;
            return a;
        }

        public static int NegLen(int n)
        {
            var a = new byte[n % 50];
            return a.Length;
        }

        public static int Narrow(byte[] b, sbyte[] s, short[] h, ushort[] u, char[] c, bool[] f)
        {
            int r = 0;
            if (b.Length > 0) { b[0] += 200; r += b[0]; }
            if (s.Length > 0) { s[0] -= 100; r += s[0]; }
            if (h.Length > 0) { h[0] *= 1000; r += h[0]; }
            if (u.Length > 0) { u[0] = (ushort)(u[0] * 7); r += u[0]; }
            if (c.Length > 0) { c[0]++; r += c[0]; }
            if (f.Length > 0) { f[0] = !f[0]; r += f[0] ? 1 : 0; }
            return r;
        }

        public static float FloatArr(float[] a, int i)
        {
            a[i & 1] = a[i & 1] * 3 + 1;
            return a[0];
        }

        public static int Objects(Node[] ns)
        {
            int s = 0;
            for (int i = 0; i < ns.Length; i++)
            {
                if (ns[i] == null)
                    ns[i] = new Node { Value = i };
                s += ns[i].Value;
            }
            return s;
        }

        public static int Statics(int i)
        {
            G.Data = new int[4];
            G.Data[i & 3] = i;
            G.Data[(i + 1) & 3] += G.Data[i & 3] + 1;
            return G.Data[3];
        }

        public static long NativeIndex(long[] a, long i) => a[i] + (long)a.Length;

        public static int ArrayOfBags(Bag[] bags, int k)
        {
            if (bags.Length == 0)
                return 0;
            bags[0].A += k;
            bags[bags.Length - 1].P.X = k;
            return bags[0].A;
        }
    }

    public class Inst
    {
        public int Total;
        public Node Head;

        public int Add(int a)
        {
            Total += a;
            return Total;
        }

        public void Push(int v)
        {
            Head = new Node { Value = v, Next = Head };
            Total++;
        }

        public int Twice(int a) => Add(a) + Add(a);

        public virtual int Hook(int a) => a + Total;
    }

    // Interfaces (translation §11, slice 5): dispatch through an interface with
    // two implementations, and an explicit implementation.
    public interface IArea
    {
        int Area();
    }

    public interface IGrow
    {
        void Grow(int k);
    }

    public class Disk : IArea, IGrow
    {
        public int R;

        public int Area() => 3 * R * R;

        void IGrow.Grow(int k) { R += k; }
    }

    public class Plate : IArea
    {
        public int W;
        public int H;

        public int Area() => W * H;
    }

    public static class Ifaces
    {
        public static int Pick(int a, int b)
        {
            IArea x = a > b ? new Disk { R = a % 1000 } : (IArea)new Plate { W = a, H = b };
            return x.Area();
        }

        public static int Explicit(int a, int k)
        {
            var d = new Disk { R = a };
            IGrow g = d;
            g.Grow(k);
            return d.R;
        }

        public static int Through(Disk d, int k)
        {
            IGrow g = d;
            g.Grow(k);
            IArea x = d;
            return x.Area() + d.R;
        }
    }

    // Delegates (translation §11, slice 9): bound to a static method (through
    // the frontend's thunk), to an instance method, to a closure, and to a
    // lambda that captures nothing (cached in a compiler-generated static).
    public delegate int IntFn(int x);

    public class Scaler
    {
        public int K;

        public int Apply(int x) => x * K;
    }

    public static class Dels
    {
        static int Twice(int x) => 2 * x;

        public static int Static(int a)
        {
            IntFn f = Twice;
            return f(a);
        }

        public static int Instance(Scaler s, int a)
        {
            IntFn f = s.Apply;
            return f(a);
        }

        // The delegate crosses a call. `Run` is a local function, compiled to a
        // `<`-named method, which FlowRun does not call on its own: it could
        // only give it a delegate it made without a constructor.
        public static int Closure(int a, int b)
        {
            IntFn f = x => x - b;
            return Run(f, a) + b;

            static int Run(IntFn f, int x) => f(x);
        }

        public static int Lambda(int a)
        {
            IntFn f = x => x + 1;
            return f(a);
        }

        public static int Field(Scaler s, int a)
        {
            var h = new Holder { Fn = s.Apply };
            return h.Fn(a);
        }
    }

    public class Holder
    {
        public IntFn Fn;
    }
}
