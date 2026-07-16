<?php
// Flow through functions
function my_source() {
    return $_POST['data']; // Source
}

function my_sink($param) {
    exec($param); // Sink
}

$x = my_source();
$a = $x;
my_sink($a);
?>
