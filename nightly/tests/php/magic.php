<?php
// Taint through PHP's magic methods: __set/__get, __call, __toString, and
// ArrayAccess element write/read.
class Bag implements ArrayAccess {
    private $data = [];

    public function __set($k, $v) {
        $this->data[$k] = $v;
    }

    public function __get($k) {
        return $this->data[$k];
    }

    public function __call($name, $args) {
        return $args[0];
    }

    public function __toString() {
        return $this->data['str'];
    }

    public function offsetSet($offset, $value): void {
        $this->data[$offset] = $value;
    }

    public function offsetGet($offset): mixed {
        return $this->data[$offset];
    }

    public function offsetExists($offset): bool {
        return isset($this->data[$offset]);
    }

    public function offsetUnset($offset): void {
        unset($this->data[$offset]);
    }
}

$tainted = $_GET['input']; // Source

$bag = new Bag();
$bag->anything = $tainted;
echo $bag->anything; // Sink

exec($bag->undefinedMethod($tainted)); // Sink

$bag['key'] = $tainted;
passthru($bag['key']); // Sink

$str = new Bag();
$str->str = $tainted;
echo $str; // Sink
?>
