<?php
// Taint through argument-passing forms: variadic parameters, argument
// unpacking, func_get_args(), default arguments, and named arguments.
$tainted = $_GET['input']; // Source

function firstOf(...$args) {
    return $args[0];
}
echo firstOf($tainted, 'clean'); // Sink

function joinAll(...$parts) {
    return implode('', $parts);
}
$spread = ['clean', $tainted];
exec(joinAll(...$spread)); // Sink

function legacy() {
    $args = func_get_args();
    return $args[1];
}
passthru(legacy('clean', $tainted)); // Sink

function withDefault($a, $b = 'clean') {
    return $b;
}
echo withDefault('x', $tainted); // Sink

function named($first = 'clean', $second = 'clean') {
    return $second;
}
echo named(second: $tainted); // Sink
?>
