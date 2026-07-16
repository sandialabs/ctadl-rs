<?php
// Taint through foreach: tainted values, tainted keys, by-reference iteration,
// and nested iteration over a tainted array of arrays.
$tainted = $_GET['input']; // Source

$values = ['a' => 'clean', 'b' => $tainted];
foreach ($values as $v) {
    echo $v; // Sink
}

$keyed = [];
$keyed[$tainted] = 'clean';
foreach ($keyed as $k => $ignored) {
    exec($k); // Sink
}

$items = ['clean', 'clean'];
foreach ($items as &$item) {
    $item = $tainted;
}
unset($item);
passthru($items[0]); // Sink

$matrix = [['inner' => $tainted]];
foreach ($matrix as $row) {
    foreach ($row as $cell) {
        echo $cell; // Sink
    }
}
?>
