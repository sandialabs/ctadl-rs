// exec.il: methods for the `cil:exec` check (see exec.cs), in shapes the C#
// compiler does not emit: values held on the stack across joins, loop back
// edges and switches; fault handlers; mutation through `unbox`; cpobj, ldobj,
// stobj and initobj through pointers; stores through ldarga; ckfinite; and
// overflow-checked native-int arithmetic.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'exec' { }
.module 'exec.il.dll'

.class public sequential ansi sealed beforefieldinit Cell extends [mscorlib]System.ValueType
{
	.field public int32 A
	.field public int64 B
}

.class public auto ansi beforefieldinit Err extends [mscorlib]System.Exception
{
	.field public int32 Code

	.method public hidebysig specialname rtspecialname instance void .ctor (int32 code) cil managed
	{
		ldarg.0
		call instance void [mscorlib]System.Exception::.ctor()
		ldarg.0
		ldarg.1
		stfld int32 Err::Code
		ret
	}
}

.class public auto ansi beforefieldinit Holder extends [mscorlib]System.Object
{
	.field public int32 N
	.field public valuetype Cell C
	.field public static int32 Count
}

.class public abstract auto ansi sealed beforefieldinit IL extends [mscorlib]System.Object
{
	// Height 2 at a join: both arms push onto a value already there.
	.method public hidebysig static int32 StackJoin (int32 a, int32 b) cil managed
	{
		ldc.i4 1000
		ldarg.1
		ldarg.0
		brtrue.s L1
		ldc.i4.2
		br.s J
	L1:	ldc.i4.3
	J:	add
		mul
		ret
	}

	// A loop whose back edge carries the running value on the stack.
	.method public hidebysig static int32 StackLoop (int32 n) cil managed
	{
		ldarg.0
		ldc.i4.7
		and
		ldc.i4.0
	L:	ldarg.0
		ldc.i4.s 15
		and
		ldc.i4.2
		add
		add
		dup
		ldc.i4 100
		bgt.s Out
		ldc.i4.1
		sub
		br.s L
	Out:	add
		ret
	}

	// A switch with a value underneath its operand.
	.method public hidebysig static int32 StackSwitch (int32 k) cil managed
	{
		ldc.i4 100
		ldarg.0
		switch (C0, C1, C2)
		ldc.i4.m1
		br.s J
	C0:	ldc.i4.1
		br.s J
	C1:	ldc.i4.2
		br.s J
	C2:	pop
		ldc.i4 -50
		ldc.i4.7
	J:	add
		ret
	}

	// dup across a conditional branch on an object reference.
	.method public hidebysig static object DupBranch (object o) cil managed
	{
		ldarg.0
		dup
		brtrue.s Done
		pop
		ldc.i4.5
		box [mscorlib]System.Int32
	Done:	ret
	}

	// newobj of a struct with values below its arguments, then a field read
	// of the struct value itself.
	.method public hidebysig static int64 NewobjDeep (int32 a, int64 b) cil managed
	{
		.locals init (valuetype Cell c)
		ldarg.1
		ldc.i4.3
		ldarg.0
		newobj instance void Cell2::.ctor(int32, int32)
		ldfld int32 Cell2::Y
		conv.i8
		add
		ret
	}

	// A fault handler runs only on the exceptional path.
	.method public hidebysig static int32 Fault (int32 a) cil managed
	{
		.locals init (int32 r)
		.try
		{
			.try
			{
				ldc.i4.s 10
				ldarg.0
				div
				stloc.0
				leave.s Inner
			}
			fault
			{
				ldsfld int32 Holder::Count
				ldc.i4.1
				add
				stsfld int32 Holder::Count
				endfinally
			}
		Inner:	leave.s Done
		}
		catch [mscorlib]System.DivideByZeroException
		{
			pop
			ldc.i4.s -9
			stloc.0
			leave.s Done
		}
	Done:	ldloc.0
		ret
	}

	// Leave from a catch through two finally blocks; the exception object
	// is still on the stack when the catch leaves.
	.method public hidebysig static int32 LeaveThroughTwo (int32 a) cil managed
	{
		.locals init (int32 r)
		.try
		{
			.try
			{
				.try
				{
					ldarg.0
					newobj instance void Err::.ctor(int32)
					throw
				}
				catch Err
				{
					dup
					ldfld int32 Err::Code
					stloc.0
					ldloc.0
					ldc.i4.0
					bge.s Positive
					leave.s Done
				Positive:	leave.s Mid
				}
			Mid:	ldloc.0
				ldc.i4.2
				mul
				stloc.0
				leave.s Done
			}
			finally
			{
				ldloc.0
				ldc.i4.s 10
				add
				stloc.0
				endfinally
			}
		}
		finally
		{
			ldloc.0
			ldc.i4 100
			mul
			stloc.0
			endfinally
		}
	Done:	ldloc.0
		ret
	}

	// A filter whose two arms meet at its one endfilter with a value on the
	// stack, and a handler that reads the exception from the stack.
	.method public hidebysig static int32 FilterStack (int32 a) cil managed
	{
		.locals init (int32 r)
		.try
		{
			ldarg.0
			newobj instance void Err::.ctor(int32)
			throw
		}
		filter
		{
			isinst Err
			dup
			brtrue.s Yes
			pop
			ldc.i4.0
			br.s End
		Yes:	ldfld int32 Err::Code
			ldc.i4.3
			rem
			ldc.i4.0
			cgt.un
		End:	endfilter
		}
		{
			castclass Err
			ldfld int32 Err::Code
			stloc.0
			leave.s Done
		}
		catch [mscorlib]System.Object
		{
			pop
			ldc.i4.m1
			stloc.0
			leave.s Done
		}
	Done:	ldloc.0
		ret
	}

	// unbox gives a pointer into the box: a store through it changes the
	// boxed value.
	.method public hidebysig static int32 UnboxMutate (int32 a) cil managed
	{
		.locals init (object o)
		ldarg.0
		box [mscorlib]System.Int32
		stloc.0
		ldloc.0
		unbox [mscorlib]System.Int32
		dup
		ldind.i4
		ldc.i4.7
		add
		stind.i4
		ldloc.0
		unbox.any [mscorlib]System.Int32
		ret
	}

	// Stores through ldarga, of each width.
	.method public hidebysig static int32 Ldarga (int32 a, uint8 b, int16 c) cil managed
	{
		ldarga.s a
		ldc.i4 1000
		stind.i4
		ldarga.s b
		ldarg.1
		ldc.i4 300
		add
		stind.i1
		ldarga.s c
		ldc.i4 70000
		stind.i2
		ldarg.0
		ldarg.1
		add
		ldarg.2
		add
		starg.s a
		ldarg.0
		ret
	}

	// cpobj, ldobj, stobj and initobj on struct locals and an array element.
	.method public hidebysig static int64 Blocks (int32 a, int64 b) cil managed
	{
		.locals init (valuetype Cell x, valuetype Cell y, valuetype Cell[] arr)
		ldloca.s x
		ldarg.0
		stfld int32 Cell::A
		ldloca.s x
		ldarg.1
		stfld int64 Cell::B
		ldloca.s y
		ldloca.s x
		cpobj Cell
		ldloca.s x
		initobj Cell
		ldloca.s x
		ldfld int64 Cell::B
		ldc.i4.2
		newarr Cell
		stloc.2
		ldloc.2
		ldc.i4.1
		ldelema Cell
		ldloca.s y
		ldobj Cell
		stobj Cell
		ldloc.2
		ldc.i4.1
		ldelema Cell
		ldfld int64 Cell::B
		ldloc.2
		ldc.i4.1
		ldelem Cell
		stloc.0
		ldloc.0
		ldfld int32 Cell::A
		conv.i8
		add
		ldloca.s x
		ldfld int64 Cell::B
		add
		add
		ret
	}

	// A struct field of an object, through ldflda, and a static counter.
	.method public hidebysig static int64 FieldAddress (class Holder h, int64 v) cil managed
	{
		.locals init (valuetype Cell c)
		ldarg.0
		ldflda valuetype Cell Holder::C
		dup
		ldfld int64 Cell::B
		ldarg.1
		add
		stfld int64 Cell::B
		ldsflda int32 Holder::Count
		dup
		ldind.i4
		ldc.i4.1
		add
		stind.i4
		ldarg.0
		ldfld valuetype Cell Holder::C
		stloc.0
		ldloca.s 0
		ldfld int64 Cell::B
		ret
	}

	.method public hidebysig static float64 Finite (float64 d) cil managed
	{
		.try
		{
			ldarg.0
			ckfinite
			ldc.r8 2
			mul
			starg.s d
			leave.s Done
		}
		catch [mscorlib]System.ArithmeticException
		{
			pop
			ldc.r8 -1
			starg.s d
			leave.s Done
		}
	Done:	ldarg.0
		ret
	}

	.method public hidebysig static int64 NativeOvf (native int a, native uint b, int64 c) cil managed
	{
		.locals init (int64 r)
		.try
		{
			ldarg.0
			ldarg.1
			add.ovf
			ldarg.0
			mul.ovf
			conv.ovf.i8
			ldarg.2
			conv.ovf.u.un
			ldarg.1
			sub.ovf.un
			conv.u8
			add
			stloc.0
			leave.s Done
		}
		catch [mscorlib]System.OverflowException
		{
			pop
			ldc.i8 -1
			stloc.0
			leave.s Done
		}
	Done:	ldloc.0
		ret
	}

	.method public hidebysig static float64 Unsigned (uint32 a, uint64 b) cil managed
	{
		ldarg.0
		conv.r.un
		ldarg.1
		conv.r.un
		add
		ldarg.0
		ldarg.1
		conv.u4
		clt.un
		conv.r8
		add
		ret
	}

	// Comparisons of object references with null and each other.
	.method public hidebysig static int32 RefCompare (object a, object b) cil managed
	{
		ldarg.0
		ldnull
		cgt.un
		ldarg.0
		ldarg.1
		ceq
		ldc.i4.2
		mul
		add
		ldarg.1
		brfalse.s Null
		ldc.i4.4
		add
	Null:	ret
	}

	// Leave out of a try nested in a catch handler, through its finally.
	.method public hidebysig static int32 TryInCatch (int32 a) cil managed
	{
		.locals init (int32 r)
		.try
		{
			ldc.i4.1
			ldarg.0
			div
			stloc.0
			leave.s Done
		}
		catch [mscorlib]System.DivideByZeroException
		{
			pop
			.try
			{
				ldc.i4.s 20
				stloc.0
				leave.s Done
			}
			finally
			{
				ldloc.0
				ldc.i4.3
				add
				stloc.0
				endfinally
			}
		}
	Done:	ldloc.0
		ret
	}

	// rethrow from a catch nested inside a finally that runs on the
	// exceptional path.
	.method public hidebysig static int32 RethrowInFinally (int32 a) cil managed
	{
		.locals init (int32 r)
		.try
		{
			.try
			{
				ldarg.0
				newobj instance void Err::.ctor(int32)
				throw
			}
			finally
			{
				.try
				{
					ldarg.0
					ldc.i4.0
					bge.s Skip
					ldc.i4.7
					newobj instance void Err::.ctor(int32)
					throw
				Skip:	leave.s FDone
				}
				catch Err
				{
					ldfld int32 Err::Code
					stloc.0
					rethrow
				}
			FDone:	endfinally
			}
		}
		catch Err
		{
			ldfld int32 Err::Code
			ldloc.0
			add
			stloc.0
			leave.s Done
		}
	Done:	ldloc.0
		ret
	}
}

.class public sequential ansi sealed beforefieldinit Cell2 extends [mscorlib]System.ValueType
{
	.field public int32 X
	.field public int32 Y

	.method public hidebysig specialname rtspecialname instance void .ctor (int32 x, int32 y) cil managed
	{
		ldarg.0
		ldarg.1
		stfld int32 Cell2::X
		ldarg.0
		ldarg.2
		ldarg.1
		add
		stfld int32 Cell2::Y
		ret
	}
}
