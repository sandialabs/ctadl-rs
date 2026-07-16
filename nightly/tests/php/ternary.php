<?php
// Taint through conditional expressions: ternary, short ternary, null
// coalescing, null-coalescing assignment, and a tainted default via ??.
$tainted = $_GET['input']; // Source
$flag = isset($_GET['flag']);

$t = $flag ? $tainted : 'clean';
echo $t; // Sink

$short = $tainted ?: 'clean';
exec($short); // Sink

$coalesce = $undefined ?? $tainted;
passthru($coalesce); // Sink

$assign = null;
$assign ??= $tainted;
echo $assign; // Sink

$direct = $_GET['maybe'] ?? 'clean'; // Source
echo $direct; // Sink
?>
