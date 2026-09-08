/**
 * A string switch inside `try`/`catch`, so its join is an exception-handler
 * edge.
 *
 * The handler entry receives exactly one exception object and discards it, so
 * it reaches the continuation with height 0, while the normal path arrives
 * carrying the two unconsumed switch selectors. The mismatch is therefore
 * reported on the handler edge, with existing_len=2 and new_len=0 -- the
 * reverse orientation of the plain string-switch case.
 */
public class GuardedStringSwitch {

    private String tag;

    public String decode(String ext) {
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
                    tag = null;
                    break;
            }
        } catch (RuntimeException e) {
            // swallowed: handler leaves the operand stack empty
        }
        return new StringBuilder().append("ext=").append(ext).toString();
    }
}
