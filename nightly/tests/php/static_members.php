<?php
// Taint through static state: static property, static method, self::,
// and late static binding via static::.
class Registry {
    public static $stash;

    public static function put($v) {
        self::$stash = $v;
    }

    public static function get() {
        return self::$stash;
    }

    public static function make($v) {
        return static::wrap($v);
    }

    public static function wrap($v) {
        return $v;
    }
}

$tainted = $_POST['data']; // Source

Registry::put($tainted);
echo Registry::get(); // Sink

Registry::$stash = $tainted;
exec(Registry::$stash); // Sink

passthru(Registry::make($tainted)); // Sink
?>
