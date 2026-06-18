class GlobalData {
    public static String data;
}

public final class CrossClassStaticFieldFlow {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        GlobalData.data = source(); // Line 10
        String local = GlobalData.data; // Line 11
        sink(local); // Line 12
    }
}
