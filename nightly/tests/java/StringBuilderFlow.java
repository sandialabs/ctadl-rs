public final class StringBuilderFlow {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        String s = source(); // Line 6
        String result = "prefix " + s + " suffix"; // Line 7
        sink(result); // Line 8
    }
}
