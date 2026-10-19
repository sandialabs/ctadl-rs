// Fixture for cil_reader::flow: what the Cecil corpus barely exercises. out/ref
// arguments, constrained. calls, delegates, multi-dimensional arrays, filters,
// cpobj/initobj, calli, prefixes, and the
// Layer 2 cases: newobj on a non-empty stack, dup + stfld, an address across
// a ?: join, and a join whose stack heights disagree. The xtask `cil:flow`
// check tests these methods by name, and its goldens hold `cil-dump --flow`
// of the whole module.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'flow' { }
.module 'flow.il.dll'

.class public sequential ansi sealed beforefieldinit Point extends [mscorlib]System.ValueType
{
	.field public int32 X
	.field public int32 Y

	.method public hidebysig specialname rtspecialname instance void .ctor (int32 x) cil managed
	{
		ldarg.0
		ldarg.1
		stfld int32 Point::X
		ret
	}
}

.class public auto ansi beforefieldinit Holder extends [mscorlib]System.Object
{
	.field public object f
	.field public int32 g
	.field public valuetype Point p
	.field public static object s

	.method public hidebysig specialname rtspecialname instance void .ctor () cil managed
	{
		ldarg.0
		call instance void [mscorlib]System.Object::.ctor()
		ret
	}

	.method public hidebysig specialname rtspecialname instance void .ctor (object o, int32 n) cil managed
	{
		ldarg.0
		call instance void [mscorlib]System.Object::.ctor()
		ldarg.0
		ldarg.1
		stfld object Holder::f
		ldarg.0
		ldarg.2
		stfld int32 Holder::g
		ret
	}

	.method public hidebysig newslot virtual instance int32 Twice (int32 x) cil managed
	{
		ldarg.1
		ldc.i4.2
		mul
		ret
	}
}

.class public auto ansi sealed Fn extends [mscorlib]System.MulticastDelegate
{
	.method public hidebysig specialname rtspecialname instance void .ctor (object 'object', native int 'method') runtime managed
	{
	}

	.method public hidebysig newslot virtual instance int32 Invoke (int32 x) runtime managed
	{
	}
}

.class public auto ansi beforefieldinit Flow extends [mscorlib]System.Object
{
	.method public static void Sink (object o) cil managed
	{
		ret
	}

	.method public static int32 Twice (int32 x) cil managed
	{
		ldarg.0
		ldc.i4.2
		mul
		ret
	}

	// int.TryParse(s, out n): the address of a local passed by reference.
	.method public static int32 OutParam (string s) cil managed
	{
		.locals init (int32 n)
		ldarg.0
		ldloca.s n
		call bool [mscorlib]System.Int32::TryParse(string, int32&)
		pop
		ldloc.0
		ret
	}

	// A write through a byref parameter: not traceable to a place.
	.method public static void Inc (int32& x) cil managed
	{
		ldarg.0
		ldarg.0
		ldind.i4
		ldc.i4.1
		add
		stind.i4
		ret
	}

	.method public static int32 CallInc () cil managed
	{
		.locals init (int32 n)
		ldloca.s n
		call void Flow::Inc(int32&)
		ldloc.0
		ret
	}

	// ldloca + struct fields: the base is a pointer to the local.
	.method public static int32 StructField () cil managed
	{
		.locals init (valuetype Point pt)
		ldloca.s pt
		ldc.i4.5
		stfld int32 Point::X
		ldloca.s pt
		ldfld int32 Point::X
		ret
	}

	// initobj, cpobj, ldobj, stobj on a struct.
	.method public static void ClearCopy (valuetype Point& dst) cil managed
	{
		.locals init (valuetype Point tmp)
		ldloca.s tmp
		initobj Point
		ldarg.0
		ldloca.s tmp
		cpobj Point
		ldarg.0
		ldobj Point
		stloc.0
		ldarg.0
		ldloc.0
		stobj Point
		ret
	}

	// A field of a struct inside an object: ldflda then ldfld.
	.method public static int32 NestedField (class Holder h) cil managed
	{
		ldarg.0
		ldflda valuetype Point Holder::p
		ldfld int32 Point::Y
		ret
	}

	// constrained. callvirt on a generic parameter.
	.method public static string Constrained<T> (!!T x) cil managed
	{
		ldarga.s x
		constrained. !!T
		callvirt instance string [mscorlib]System.Object::ToString()
		ret
	}

	// A delegate over an instance method, invoked.
	.method public static int32 Delegates (class Holder h) cil managed
	{
		ldarg.0
		ldftn instance int32 Holder::Twice(int32)
		newobj instance void Fn::.ctor(object, native int)
		ldc.i4.3
		callvirt instance int32 Fn::Invoke(int32)
		ret
	}

	// A lambda: a static method under a null target, and a virtual one.
	.method public static class Fn Lambdas (class Holder h) cil managed
	{
		ldnull
		ldftn int32 Flow::Twice(int32)
		newobj instance void Fn::.ctor(object, native int)
		pop
		ldarg.0
		dup
		ldvirtftn instance int32 Holder::Twice(int32)
		newobj instance void Fn::.ctor(object, native int)
		ret
	}

	.method public static int32 MultiArray () cil managed
	{
		.locals init (int32[0..., 0...] a)
		ldc.i4.2
		ldc.i4.3
		newobj instance void int32[0..., 0...]::.ctor(int32, int32)
		stloc.0
		ldloc.0
		ldc.i4.1
		ldc.i4.2
		ldc.i4.7
		call instance void int32[0..., 0...]::Set(int32, int32, int32)
		ldloc.0
		ldc.i4.1
		ldc.i4.2
		call instance int32 int32[0..., 0...]::Get(int32, int32)
		pop
		ldloc.0
		ldc.i4.1
		ldc.i4.2
		call instance int32& int32[0..., 0...]::Address(int32, int32)
		ldind.i4
		ret
	}

	.method public static void Filter (object o) cil managed
	{
		.try
		{
			ldarg.0
			call void Flow::Sink(object)
			leave.s END
		}
		filter
		{
			isinst [mscorlib]System.ArgumentException
			ldnull
			cgt.un
			endfilter
		}
		{
			call void Flow::Sink(object)
			leave.s END
		}
	END:
		ret
	}

	// catch (Exception e) { Sink(e.Message); throw; } inside try/finally.
	.method public static void CatchFinally (object o) cil managed
	{
		.locals init (class [mscorlib]System.Exception e)
		.try
		{
			.try
			{
				ldarg.0
				call void Flow::Sink(object)
				leave.s INNER
			}
			catch [mscorlib]System.Exception
			{
				stloc.0
				ldloc.0
				callvirt instance string [mscorlib]System.Exception::get_Message()
				call void Flow::Sink(object)
				rethrow
			}
		INNER:
			leave.s END
		}
		finally
		{
			ldarg.0
			call void Flow::Sink(object)
			endfinally
		}
	END:
		ret
	}

	.method public static int32 Calli (int32 x) cil managed
	{
		ldarg.0
		ldftn int32 Flow::Twice(int32)
		calli int32(int32)
		ret
	}

	// newobj with arguments on a stack that is not empty: the new object
	// would land on its first argument's depth.
	.method public static object NewobjArgs (object o) cil managed
	{
		ldnull
		ldarg.0
		ldc.i4.1
		newobj instance void Holder::.ctor(object, int32)
		stsfld object Holder::s
		ret
	}

	// new Holder { f = o }: dup + stfld.
	.method public static class Holder DupStfld (object o) cil managed
	{
		newobj instance void Holder::.ctor()
		dup
		ldarg.0
		stfld object Holder::f
		ret
	}

	// *(c ? &a : &b) = 5, then *(c ? &a : &a) = 6: the first address is
	// two places, the second one.
	.method public static int32 AddressJoin (bool c) cil managed
	{
		.locals init (int32 a, int32 b)
		ldarg.0
		brtrue.s A1
		ldloca.s a
		br.s J1
	A1:
		ldloca.s b
	J1:
		ldc.i4.5
		stind.i4
		ldarg.0
		brtrue.s A2
		ldloca.s a
		br.s J2
	A2:
		ldloca.s a
	J2:
		ldc.i4.6
		stind.i4
		ldloc.0
		ldloc.1
		add
		ret
	}

	// Unverifiable: the two paths reach J with heights 2 and 1.
	.method public static void HeightMismatch (bool c) cil managed
	{
		ldarg.0
		brtrue.s A
		ldc.i4.1
		ldc.i4.2
		br.s J
	A:
		ldc.i4.3
	J:
		pop
		ret
	}

	.method public static vararg void VarArgs (int32 n) cil managed
	{
		arglist
		pop
		ret
	}

	.method public static void CallVarArgs () cil managed
	{
		ldc.i4.1
		ldstr "x"
		ldc.i4.2
		call vararg void Flow::VarArgs(int32, ..., string, int32)
		ret
	}

	.method public static void Blocks (native int src) cil managed
	{
		.locals init (native int p)
		ldc.i4.s 16
		localloc
		stloc.0
		ldloc.0
		ldc.i4.0
		ldc.i4.s 16
		initblk
		ldloc.0
		ldarg.0
		ldc.i4.s 16
		unaligned. 1
		cpblk
		ret
	}

	.method public static int32 Prefixes (class Holder h, int32[] a) cil managed
	{
		ldarg.1
		ldc.i4.0
		readonly.
		ldelema int32
		pop
		ldarg.0
		volatile.
		ldfld int32 Holder::g
		tail.
		call int32 Flow::Twice(int32)
		ret
	}

	.method public static object Misc (object o, int32 i, float64 d) cil managed
	{
		.locals init (int32[] arr, valuetype Point pt, typedref tr)
		ldarg.1
		switch (L0, L1)
	L0:
		ldarg.1
		newarr [mscorlib]System.Int32
		stloc.0
		ldloc.0
		ldarg.1
		ldarg.1
		stelem.i4
		ldloc.0
		ldlen
		conv.i4
		ldloc.0
		ldc.i4.0
		ldelem.i4
		add
		box [mscorlib]System.Int32
		unbox.any [mscorlib]System.Int32
		pop
		ldarg.0
		castclass [mscorlib]System.String
		isinst [mscorlib]System.String
		unbox.any [mscorlib]System.String
		pop
		ldarg.2
		ckfinite
		conv.ovf.i4
		pop
		ldloca.s pt
		mkrefany Point
		stloc.2
		ldloc.2
		refanyval Point
		pop
		ldloc.2
		refanytype
		pop
		ldtoken Point
		pop
		sizeof Point
		pop
	L1:
		ldarg.0
		ret
	}

	.method public static void Jump (object o) cil managed
	{
		jmp void Flow::Sink(object)
	}
}
