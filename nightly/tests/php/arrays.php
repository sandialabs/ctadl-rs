<?php
// Taint through arrays: element write/read, whole-array copy, nesting,
// list() destructuring, and append.
$tainted = $_GET['payload']; // Source

$arr = [];
$arr['evil'] = $tainted;
$arr['safe'] = 'constant';

$copy = $arr;
echo $copy['evil']; // Sink

$nested = ['outer' => ['inner' => $tainted]];
exec($nested['outer']['inner']); // Sink

$pair = [$tainted, 'clean'];
[$first, $second] = $pair;
passthru($first); // Sink

$appended = [];
$appended[] = $tainted;
echo $appended[0]; // Sink
?>
