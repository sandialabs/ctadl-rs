<?php
// Taint through polymorphic dispatch: an interface with two implementations, an
// abstract base, and a trait -- the sink is reached through the interface type.
interface Echoer {
    public function emit($v);
}

abstract class BaseEchoer implements Echoer {
    public function emitTwice($v) {
        $this->emit($v);
        $this->emit($v);
    }
}

trait Prefixes {
    public function prefix($v) {
        return 'p:' . $v;
    }
}

class DirectEchoer extends BaseEchoer {
    public function emit($v) {
        echo $v; // Sink
    }
}

class ShellEchoer extends BaseEchoer {
    use Prefixes;

    public function emit($v) {
        exec($this->prefix($v)); // Sink
    }
}

$tainted = $_GET['input']; // Source

$impls = [new DirectEchoer(), new ShellEchoer()];
foreach ($impls as $impl) {
    $impl->emit($tainted);
}

$base = new ShellEchoer();
$base->emitTwice($tainted);
?>
