<?php
// Access path test
function get_nested($obj) {
    // summary: parameter 0.foo.bar flows to return value
    return $obj->foo->bar;
}

$tainted = $_GET['input']; // Source
$o = new stdClass();
$o->foo = new stdClass();
$o->foo->bar = $tainted;

$res = get_nested($o);
passthru($res); // Sink
?>
