<?php
// Taint through PHP references: reference assignment, reference parameters,
// references into arrays, and reference to an object property.
$tainted = $_GET['input']; // Source

$alias = &$tainted;
echo $alias; // Sink

function fill(&$out, $v) {
    $out = $v;
}
$filled = 'clean';
fill($filled, $tainted);
exec($filled); // Sink

$arr = ['slot' => 'clean'];
$slot = &$arr['slot'];
$slot = $tainted;
passthru($arr['slot']); // Sink

$obj = new stdClass();
$obj->prop = 'clean';
$propRef = &$obj->prop;
$propRef = $tainted;
echo $obj->prop; // Sink

function swapIn(&$target) {
    $target = $_POST['later']; // Source
}
$swapped = 'clean';
swapIn($swapped);
echo $swapped; // Sink
?>
