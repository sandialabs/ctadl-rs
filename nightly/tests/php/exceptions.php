<?php
// Taint carried by exceptions: through a built-in Exception message, through a
// property on a custom exception, and out of a finally block.
class TaintedException extends Exception {
    public $payload;

    public function __construct($payload) {
        parent::__construct('failed');
        $this->payload = $payload;
    }
}

$tainted = $_GET['input']; // Source

try {
    throw new Exception($tainted);
} catch (Exception $e) {
    echo $e->getMessage(); // Sink
}

try {
    throw new TaintedException($tainted);
} catch (TaintedException $e) {
    exec($e->payload); // Sink
}

$leaked = 'clean';
try {
    $leaked = $tainted;
    throw new Exception('boom');
} catch (Exception $e) {
    // swallowed
} finally {
    passthru($leaked); // Sink
}
?>
