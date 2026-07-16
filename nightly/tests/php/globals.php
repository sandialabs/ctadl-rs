<?php
// Taint through global scope: the `global` keyword, the $GLOBALS array, and a
// function-local `static` that carries taint across calls.
$g_tainted = $_GET['input']; // Source
$g_sink = 'clean';

function readsGlobal() {
    global $g_tainted;
    return $g_tainted;
}
echo readsGlobal(); // Sink

function writesGlobal() {
    global $g_tainted, $g_sink;
    $g_sink = $g_tainted;
}
writesGlobal();
exec($g_sink); // Sink

function viaGlobalsArray() {
    return $GLOBALS['g_tainted'];
}
passthru(viaGlobalsArray()); // Sink

function remembers($v = null) {
    static $held = 'clean';
    if ($v !== null) {
        $held = $v;
    }
    return $held;
}
remembers($g_tainted);
echo remembers(); // Sink
?>
