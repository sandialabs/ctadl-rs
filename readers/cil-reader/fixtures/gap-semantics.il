// Property and event accessors as Cecil associates them, and property
// names without accessors.

.assembly extern mscorlib { .publickeytoken = (B7 7A 5C 56 19 34 E0 89) .ver 4:0:0:0 }
.assembly 'gap-semantics' { }
.module 'gap-semantics.il.dll'

.class public auto ansi beforefieldinit Shared extends [mscorlib]System.Object
{
	.method public specialname instance int32 get_X () cil managed { ldc.i4.0 ret }
	.method public specialname instance void set_X (int32 v) cil managed { ret }
	.method public specialname instance void add_E (class [mscorlib]System.EventHandler h) cil managed { ret }
	.method public specialname instance void remove_E (class [mscorlib]System.EventHandler h) cil managed { ret }
	.method public instance void O1 () cil managed { ret }
	.method public instance void O2 () cil managed { ret }
	.method public instance void O3 () cil managed { ret }

	// One getter for two properties: Cecil keeps one semantics row per
	// method, the last, so only P2 gets it.
	.property instance int32 P1 () { .get instance int32 Shared::get_X () }
	.property instance int32 P2 () { .get instance int32 Shared::get_X () }

	// The same for a setter, and for an event's adder and remover.
	.property instance int32 Q1 () { .set instance void Shared::set_X (int32) }
	.property instance int32 Q2 () { .set instance void Shared::set_X (int32) }
	.event [mscorlib]System.EventHandler E1 {
		.addon instance void Shared::add_E (class [mscorlib]System.EventHandler)
		.removeon instance void Shared::remove_E (class [mscorlib]System.EventHandler)
	}
	.event [mscorlib]System.EventHandler E2 {
		.addon instance void Shared::add_E (class [mscorlib]System.EventHandler)
		.removeon instance void Shared::remove_E (class [mscorlib]System.EventHandler)
	}

	// Other methods listed out of method order: Cecil lists them in method
	// order.
	.property instance int32 Others () {
		.other instance void Shared::O3 ()
		.other instance void Shared::O1 ()
		.other instance void Shared::O2 ()
	}
	.event [mscorlib]System.EventHandler EOthers {
		.addon instance void Shared::add_E (class [mscorlib]System.EventHandler)
		.removeon instance void Shared::remove_E (class [mscorlib]System.EventHandler)
		.other instance void Shared::O2 ()
		.other instance void Shared::O1 ()
	}
}

.class public auto ansi beforefieldinit Owner extends [mscorlib]System.Object
{
	// Indexer signatures with no getter or setter: Cecil's Parameters come
	// from the accessors, so the name has "()".
	.property instance int32 NoAccessors (int32, string) { }

	.method public instance void Other () cil managed { ret }
	.property instance int32 OnlyOther (int32) { .other instance void Owner::Other () }
}
