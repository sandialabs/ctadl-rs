/**
 * A Java 8 string switch: both switch instructions at once, and -- in the
 * second method -- a join that is also an exception-handler edge.
 *
 * A Java 8 string switch lowers to two JVM switches: a lookupswitch on
 * String.hashCode() followed by a tableswitch on the synthetic index. Both
 * consume a selector. A decoder that models neither reaches the common
 * continuation with two phantom slots.
 *
 * Mirrors brut.androlib.res.decoder.ResFileDecoder.decode.
 */
public class StringSwitch {

    private String tag;

    private String decode(String ext) {
        switch (ext) {                 // lookupswitch on hashCode, then tableswitch
            case "raw":
                tag = "raw";
                break;
            case "xml":
                tag = "xml";
                break;
            case "9.png":
                tag = "9.png";
                break;
            default:
                break;
        }
        // Join point reached from the tableswitch default arm and from the
        // hash-mismatch branches inside the lookupswitch arms. StackMapTable
        // says the stack is empty here.
        return new StringBuilder().append("ext=").append(ext).toString();
    }

    /**
     * Same string switch, but the continuation is also reachable from an
     * exception handler that pops its exception object and falls through. The
     * handler arrives with height 0 while the normal path arrives with the two
     * phantom slots, so the mismatch surfaces on the handler edge.
     */
    private String decodeGuarded(String ext) {
        try {
            switch (ext) {
                case "raw":
                    tag = "raw";
                    break;
                case "xml":
                    tag = "xml";
                    break;
                case "9.png":
                    tag = "9.png";
                    break;
                default:
                    break;
            }
        } catch (RuntimeException e) {  // handler entry: one exception object
            // discarded, so the handler leaves the stack empty
        }
        return new StringBuilder().append("ext=").append(ext).toString();
    }

    public String run(String ext) {
        return decode(ext) + decodeGuarded(ext);
    }
}
