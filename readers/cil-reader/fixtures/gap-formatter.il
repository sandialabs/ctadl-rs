// Operands the Cecil test Formatter renders.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'gap-formatter' { }
.module 'gap-formatter.il.dll'

.class public auto ansi beforefieldinit Floats extends [mscorlib]System.Object
{
	// Single and double constants: .NET's shortest round-trip text, with
	// its switch to exponent form, the sign of zero, and the specials.
	.method public static void Singles () cil managed
	{
		ldc.r4 0.1
		ldc.r4 -0.0
		ldc.r4 1.5
		ldc.r4 1e-5
		ldc.r4 1e-4
		ldc.r4 1e7
		ldc.r4 1e8
		ldc.r4 16777216.0
		ldc.r4 123456789.0
		ldc.r4 3.4028235e38
		ldc.r4 1.17549435e-38
		ldc.r4 1.4e-45
		ldc.r4 float32(0x7fc00000)
		ldc.r4 float32(0xffc00000)
		ldc.r4 float32(0x7f800000)
		ldc.r4 float32(0xff800000)
		ret
	}

	.method public static void Doubles () cil managed
	{
		ldc.r8 0.1
		ldc.r8 -0.0
		ldc.r8 -1.25
		ldc.r8 1e-5
		ldc.r8 1e-4
		ldc.r8 0.0001234
		ldc.r8 1e14
		ldc.r8 1e15
		ldc.r8 1e16
		ldc.r8 1e17
		ldc.r8 123456789012345680.0
		ldc.r8 9007199254740993.0
		ldc.r8 1.7976931348623157e308
		ldc.r8 2.2250738585072014e-308
		ldc.r8 5e-324
		ldc.r8 0.30000000000000004
		ldc.r8 float64(0x7ff8000000000000)
		ldc.r8 float64(0x7ff0000000000000)
		ldc.r8 float64(0xfff0000000000000)
		ret
	}
}

.class public auto ansi beforefieldinit Args extends [mscorlib]System.Object
{
	// Argument and local operands past the end of the signature or the
	// locals: Cecil resolves them to null and prints no operand.
	.method public static void Static (int32 a, int32 b) cil managed
	{
		.locals init (int32 x)
		ldarg.s 1
		ldarg.s 2
		ldarg 7
		ldarga.s 2
		starg.s 3
		ldloc.s 0
		ldloc.s 1
		ldloc 4
		ldloca.s 1
		stloc.s 2
		ret
	}

	.method public instance void Instance (int32 a) cil managed
	{
		ldarg.s 0
		ldarg.s 1
		ldarg.s 2
		starg.s 5
		ldloc.s 0
		ret
	}

	.method public static void NoLocals () cil managed
	{
		ldarg.s 0
		ldloc.s 0
		ret
	}
}
