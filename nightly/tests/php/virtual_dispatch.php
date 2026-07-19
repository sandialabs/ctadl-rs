<?php
// Guards receiver precision on a method call whose name is ambiguous.
//
// Two unrelated classes declare a method with the same name, `run`. The only
// object ever built is a `Safe`, so `$obj->run($tainted)` can only dispatch to
// `Safe::run`, and the taint must reach solely that method's `echo` sink.
//
// A name-only resolver would make every method named `run` a candidate target
// regardless of the receiver's type, routing the taint into `Danger::run`'s
// `exec` sink as well -- a sink no receiver in this program can ever reach. That
// false positive must NOT appear: `Safe::run`'s echo is `expected_lines` and
// `Danger::run`'s exec is `unexpected_lines` in the query, so this case fails if
// the taint ever reaches `Danger::run`.

class Safe {
    public function run($v) {
        echo $v; // Sink (real target -- expected)
    }
}

class Danger {
    public function run($v) {
        exec($v); // Sink (unreachable receiver -- must stay clean, unexpected_lines)
    }
}

$tainted = $_GET['input']; // Source

$obj = new Safe();
$obj->run($tainted);
?>
