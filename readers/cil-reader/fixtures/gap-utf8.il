// Custom-attribute strings that are not valid UTF-8. Cecil decodes them
// with Encoding.UTF8, which turns each maximal invalid subsequence into
// U+FFFD.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'gap-utf8' { }
.module 'gap-utf8.il.dll'

.class public auto ansi beforefieldinit Attr extends [mscorlib]System.Attribute
{
	.field public string F
	.method public hidebysig specialname rtspecialname instance void .ctor () cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (string s) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (class [mscorlib]System.Type t) cil managed { ret }
	.method public hidebysig specialname instance void set_P (string v) cil managed { ret }
	.property instance string P () { .set instance void Attr::set_P (string) }
}

.class public auto ansi beforefieldinit Uses extends [mscorlib]System.Object
{
	// "a", an encoded surrogate (ED A0 80), "b".
	.custom instance void Attr::.ctor (string) = ( 01 00 05 61 ED A0 80 62 00 00 )
	// A lone continuation byte, then a sequence cut short by the end.
	.custom instance void Attr::.ctor (string) = ( 01 00 03 80 E2 82 00 00 )
	// A typeof name with an overlong encoding of NUL (C0 80).
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 03 58 C0 80 00 00 )
	// A named field whose name has a byte that never starts a sequence.
	.custom instance void Attr::.ctor () = ( 01 00 01 00 53 0E 02 46 F8 01 61 )
	// A named property whose value is a cut-short four-byte sequence.
	.custom instance void Attr::.ctor () = ( 01 00 01 00 54 0E 01 50 03 F0 9F 98 )
}
