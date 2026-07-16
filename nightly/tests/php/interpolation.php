<?php
// Taint through string building: concatenation, interpolation, braced
// interpolation, heredoc, compound append, and sprintf.
$name = $_GET['name']; // Source

$concat = 'Hello, ' . $name . '!';
echo $concat; // Sink

$interp = "Welcome back, $name";
echo $interp; // Sink

$braced = "id={$name}";
echo $braced; // Sink

$doc = <<<HTML
<p>$name</p>
HTML;
echo $doc; // Sink

$appended = 'prefix';
$appended .= $name;
echo $appended; // Sink

$fmt = sprintf('user=%s', $name);
echo $fmt; // Sink
?>
