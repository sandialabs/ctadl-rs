public final class FieldSensitivity {
    String tainted;
    String safe;

    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        FieldSensitivity obj = new FieldSensitivity();
        obj.tainted = source();
        obj.safe = "safe";
        
        sink(obj.tainted); // Line 13
        sink(obj.safe);    // Line 14
    }
}
