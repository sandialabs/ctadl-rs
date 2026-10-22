// Reassignment.java
public final class Reassignment {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        String data = source(); // Line 7
        data = "safe";          // Line 8
        sink(data);             // Line 9
    }
}
