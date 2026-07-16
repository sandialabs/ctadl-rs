<?php
// Taint through closures: captured by value, captured by reference, arrow
// function capture, closure passed as an argument, and a callable variable.
$tainted = $_GET['input']; // Source

$byValue = function () use ($tainted) {
    return $tainted;
};
echo $byValue(); // Sink

$out = 'clean';
$byRef = function () use ($tainted, &$out) {
    $out = $tainted;
};
$byRef();
exec($out); // Sink

$arrow = fn() => $tainted;
passthru($arrow()); // Sink

$param = function ($v) {
    return $v;
};
echo $param($tainted); // Sink

function apply($fn, $arg) {
    return $fn($arg);
}
echo apply($param, $tainted); // Sink

$callable = 'strval';
echo $callable($tainted); // Sink
?>
