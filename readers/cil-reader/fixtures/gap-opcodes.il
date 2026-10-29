// Bytes that name no opcode but fall inside Cecil's opcode tables. Cecil
// reads each as nop with a 4-byte branch operand and keeps decoding.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'gap-opcodes' { }
.module 'gap-opcodes.il.dll'

.class public auto ansi beforefieldinit Undefined extends [mscorlib]System.Object
{
	// One-byte holes, each with a branch operand: to the next instruction,
	// back to the start, and past the end of the code.
	.method public static void OneByte () cil managed
	{
		nop
		.emitbyte 0x24
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0xa6
		.emitbyte 0xf5
		.emitbyte 0xff
		.emitbyte 0xff
		.emitbyte 0xff
		.emitbyte 0xcf
		.emitbyte 0x00
		.emitbyte 0x01
		.emitbyte 0x00
		.emitbyte 0x00
		ret
	}

	// Two-byte holes after the 0xfe prefix, the first table entry, the
	// one before readonly., and one in between.
	.method public static void TwoByte () cil managed
	{
		.emitbyte 0xfe
		.emitbyte 0x08
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0xfe
		.emitbyte 0x10
		.emitbyte 0x01
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0xfe
		.emitbyte 0x1b
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		.emitbyte 0x00
		nop
		ret
	}

	// A hole's operand swallows the bytes after it: here the first four
	// bytes of the ldc.i4, so its last byte reads as a nop.
	.method public static int32 Swallowed () cil managed
	{
		.emitbyte 0x77
		ldc.i4 7
		ret
	}
}
