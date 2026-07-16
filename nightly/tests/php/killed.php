<?php
// Negative test: every tainted value is overwritten or dropped before any sink
// runs, so no flow may be reported at all.
$tainted = $_GET['input']; // Source

// Strong update: the tainted value is gone by the time it is echoed.
$overwritten = $tainted;
$overwritten = 'constant';
echo $overwritten;

// Only the untainted field of the array is read.
$arr = ['evil' => $tainted, 'safe' => 'constant'];
exec($arr['safe']);

// The function drops its argument.
function ignores($v) {
    return 'constant';
}
passthru(ignores($tainted));

// The tainted branch never reaches the sink.
if (false) {
    $dead = $tainted;
}
echo 'constant';
?>
