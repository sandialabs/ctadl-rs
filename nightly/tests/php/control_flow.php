<?php
// Taint that survives control flow: a tainted branch of if/else, a loop-carried
// value, a switch arm, and a do-while.
$tainted = $_GET['input']; // Source
$flag = isset($_GET['flag']);

if ($flag) {
    $branch = $tainted;
} else {
    $branch = 'clean';
}
echo $branch; // Sink

$loop = 'clean';
for ($i = 0; $i < 3; $i++) {
    $loop = $tainted;
}
exec($loop); // Sink

$acc = '';
while ($flag) {
    $acc .= $tainted;
    break;
}
passthru($acc); // Sink

switch ($flag) {
    case true:
        $picked = $tainted;
        break;
    default:
        $picked = 'clean';
}
echo $picked; // Sink

$n = 0;
do {
    $carried = $tainted;
    $n++;
} while ($n < 2);
echo $carried; // Sink
?>
