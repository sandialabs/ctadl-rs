<?php
// Taint stored in object state: constructor, public property, getter/setter,
// a sink inside a method, and an inherited method.
class Holder {
    public $value;
    private $secret;

    public function __construct($v) {
        $this->value = $v;
    }

    public function getValue() {
        return $this->value;
    }

    public function setSecret($s) {
        $this->secret = $s;
    }

    public function leakSecret() {
        echo $this->secret; // Sink
    }
}

class SubHolder extends Holder {
    public function decorate() {
        return '[' . $this->getValue() . ']';
    }
}

$tainted = $_GET['input']; // Source

$h = new Holder($tainted);
echo $h->getValue(); // Sink

$direct = new Holder('clean');
$direct->value = $tainted;
exec($direct->value); // Sink

$h->setSecret($tainted);
$h->leakSecret();

$s = new SubHolder($tainted);
passthru($s->decorate()); // Sink
?>
