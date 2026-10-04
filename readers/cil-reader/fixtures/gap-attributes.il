// Custom-attribute blobs and field data that depend on how types are named.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'gap-attributes' { .ver 1:2:3:4 }
.module 'gap-attributes.il.dll'

.class public auto ansi sealed Bingo extends [mscorlib]System.Enum
{
	.field public specialname rtspecialname int16 value__
	.field public static literal valuetype Bingo Fuel = int16(0x0402)
}

.class public auto ansi beforefieldinit Attr extends [mscorlib]System.Attribute
{
	.field public object Field

	.method public hidebysig specialname rtspecialname instance void .ctor () cil managed { ret }
	// Primitives written as type references, as ilasm and other
	// non-Roslyn producers do: Cecil maps a System TypeRef with an
	// assembly scope to the primitive.
	.method public hidebysig specialname rtspecialname instance void .ctor (valuetype [mscorlib]System.Int32 a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (class [mscorlib]System.String a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (class [mscorlib]System.Object a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (valuetype [mscorlib]System.Boolean a, valuetype [mscorlib]System.Int64 b) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (valuetype [mscorlib]System.Int32[] a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (class [mscorlib]System.Type a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (valuetype NotEnum a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (valuetype ObjectEnum a) cil managed { ret }
	.method public hidebysig specialname rtspecialname instance void .ctor (valuetype ArrayEnum a) cil managed { ret }
}

// Argument types Cecil refuses to read as enums: a struct is not an enum,
// and an enum's underlying type must be a primitive.
.class public sequential ansi sealed NotEnum extends [mscorlib]System.ValueType
{
	.field public int16 value__
}

.class public auto ansi sealed ObjectEnum extends [mscorlib]System.Enum
{
	.field public specialname rtspecialname object value__
}

.class public auto ansi sealed ArrayEnum extends [mscorlib]System.Enum
{
	.field public specialname rtspecialname int32[] value__
}

.class public auto ansi beforefieldinit NotEnums extends [mscorlib]System.Object
{
	.custom instance void Attr::.ctor (valuetype NotEnum) = ( 01 00 2A 00 00 00 )
	.custom instance void Attr::.ctor (valuetype ObjectEnum) = ( 01 00 08 07 00 00 00 00 00 )
	.custom instance void Attr::.ctor (valuetype ArrayEnum) = ( 01 00 01 00 00 00 07 00 00 00 00 00 )
}

.class public auto ansi beforefieldinit Uses extends [mscorlib]System.Object
{
	.custom instance void Attr::.ctor (valuetype [mscorlib]System.Int32) = ( 01 00 2A 00 00 00 00 00 )
	.custom instance void Attr::.ctor (class [mscorlib]System.String) = ( 01 00 03 61 62 63 00 00 )
	.custom instance void Attr::.ctor (class [mscorlib]System.Object) = ( 01 00 08 07 00 00 00 00 00 )
	.custom instance void Attr::.ctor (valuetype [mscorlib]System.Boolean, valuetype [mscorlib]System.Int64) = ( 01 00 01 09 00 00 00 00 00 00 00 00 00 )
	.custom instance void Attr::.ctor (valuetype [mscorlib]System.Int32[]) = ( 01 00 02 00 00 00 01 00 00 00 02 00 00 00 00 00 )

	// A named argument of enum type 0x55, its type name qualified with
	// this assembly: Cecil takes the current module only on an exact
	// full-name match.
	// "Bingo"
	.custom instance void Attr::.ctor () = ( 01 00 01 00 53 55 05 42 69 6E 67 6F 05 46 69 65 6C 64 02 04 )
	// "Bingo, gap-attributes, Version=1.2.3.4, Culture=neutral, PublicKeyToken=null"
	.custom instance void Attr::.ctor () = ( 01 00 01 00 53 55 4C
		42 69 6E 67 6F 2C 20 67 61 70 2D 61 74 74 72 69 62 75 74 65 73 2C 20 56 65 72 73 69 6F 6E 3D 31 2E 32 2E 33 2E 34 2C 20 43 75 6C 74 75 72 65 3D 6E 65 75 74 72 61 6C 2C 20 50 75 62 6C 69 63 4B 65 79 54 6F 6B 65 6E 3D 6E 75 6C 6C
		05 46 69 65 6C 64 02 04 )
	// "Bingo, gap-attributes, Version=9.9.9.9, Culture=neutral, PublicKeyToken=null"
	.custom instance void Attr::.ctor () = ( 01 00 01 00 53 55 4C
		42 69 6E 67 6F 2C 20 67 61 70 2D 61 74 74 72 69 62 75 74 65 73 2C 20 56 65 72 73 69 6F 6E 3D 39 2E 39 2E 39 2E 39 2C 20 43 75 6C 74 75 72 65 3D 6E 65 75 74 72 61 6C 2C 20 50 75 62 6C 69 63 4B 65 79 54 6F 6B 65 6E 3D 6E 75 6C 6C
		05 46 69 65 6C 64 02 04 )
	// "Bingo, gap-attributes"
	.custom instance void Attr::.ctor () = ( 01 00 01 00 53 55 15
		42 69 6E 67 6F 2C 20 67 61 70 2D 61 74 74 72 69 62 75 74 65 73
		05 46 69 65 6C 64 02 04 )
}

.class public auto ansi beforefieldinit TypeNames extends [mscorlib]System.Object
{
	// typeof names whose arity C#'s int.TryParse reads: surrounding
	// whitespace and a sign.
	// "A` 1[[System.Int32]]"
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 14 41 60 20 31 5B 5B 53 79 73 74 65 6D 2E 49 6E 74 33 32 5D 5D 00 00 )
	// "A`1 [[System.Int32]]"
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 14 41 60 31 20 5B 5B 53 79 73 74 65 6D 2E 49 6E 74 33 32 5D 5D 00 00 )
	// "A`+1[[System.Int32]]"
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 14 41 60 2B 31 5B 5B 53 79 73 74 65 6D 2E 49 6E 74 33 32 5D 5D 00 00 )
	// "A`-1+B`2[[System.Int32]]"
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 18 41 60 2D 31 2B 42 60 32 5B 5B 53 79 73 74 65 6D 2E 49 6E 74 33 32 5D 5D 00 00 )
	// "A`4294967297+B`1[[System.Int32]]"
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 20 41 60 34 32 39 34 39 36 37 32 39 37 2B 42 60 31 5B 5B 53 79 73 74 65 6D 2E 49 6E 74 33 32 5D 5D 00 00 )
	// "A`01[[System.Int32]]"
	.custom instance void Attr::.ctor (class [mscorlib]System.Type) = ( 01 00 14 41 60 30 31 5B 5B 53 79 73 74 65 6D 2E 49 6E 74 33 32 5D 5D 00 00 )
}

// Field data is sized by the field type; Cecil reports RVA 0 when it
// cannot size it.
.class public sequential ansi sealed beforefieldinit NoLayout extends [mscorlib]System.ValueType
{
	.field public int32 x
}

.class public sequential ansi sealed beforefieldinit Sized`1<T> extends [mscorlib]System.ValueType
{
	.pack 0
	.size 6
	.field public int32 x
}

.class public auto ansi beforefieldinit Data extends [mscorlib]System.Object
{
	.field public static valuetype [mscorlib]System.Int32 RefInt32 at D_0
	.field public static valuetype [mscorlib]System.Int16 RefInt16 at D_0
	.field public static valuetype NoLayout NoLayout at D_0
	.field public static class Data Class at D_0
	.field public static valuetype Sized`1<int32> Generic at D_0
	.field public static int32 Plain at D_0
}

.data D_0 = bytearray (01 02 03 04 05 06 07 08)
