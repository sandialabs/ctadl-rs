// StringSwitchFlow.java
//
// A Java 8 string switch lowers to *both* switch instructions -- a
// `lookupswitch` on `hashCode()` followed by a `tableswitch` on the matched
// index -- so an unmodelled selector leaves two phantom slots at the join.
// `guarded` wraps the same shape in try/catch, which makes the join an
// exception-handler edge as well: the handler arrives with only its exception
// object where the normal path has two.
public final class StringSwitchFlow {

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    static String byName(String key) {
        String held = "clean";
        switch (key) {
            case "alpha":
                held = source();
                break;
            case "beta":
                held = "b";
                break;
            case "gamma":
                held = "c";
                break;
            default:
                held = "d";
                break;
        }
        return held;
    }

    static String guarded(String key) {
        String held = "clean";
        try {
            switch (key) {
                case "alpha":
                    held = source();
                    break;
                case "beta":
                    held = "b";
                    break;
                default:
                    held = "d";
                    break;
            }
        } catch (RuntimeException e) {
            held = "caught";
        }
        return held;
    }

    public static void main(String[] args) {
        sink(byName("alpha"));
        sink(guarded("alpha"));
    }
}
