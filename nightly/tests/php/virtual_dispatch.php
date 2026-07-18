<?php
// Pins the imprecision of PHP's name-based call resolution.
//
// Two unrelated classes declare a method with the same name, `run`. The only
// object ever built is a `Safe`, so `$obj->run($tainted)` can only dispatch to
// `Safe::run` -- a receiver-sensitive analysis (the way `JavaCall` resolves
// through the `resolvent` lattice) routes the taint solely into `Safe::run`'s
// `echo` sink.
//
// The current PHP path in codegen resolves a method call by *name alone*: it
// makes every method named `run` a candidate target regardless of the
// receiver's type. So it also routes the taint into `Danger::run`'s `exec`
// sink, which no receiver in this program can ever reach. That false positive
// is what this case documents -- see `unexpected_lines` in the query. It is
// marked `xfail` until PHP method calls are wired into the receiver-sensitive
// dispatch machinery.

class Safe {
    public function run($v) {
        echo $v; // Sink (real target -- expected)
    }
}

class Danger {
    public function run($v) {
        exec($v); // Sink (unreachable receiver -- must stay clean)
    }
}

$tainted = $_GET['input']; // Source

$obj = new Safe();
$obj->run($tainted);
?>
