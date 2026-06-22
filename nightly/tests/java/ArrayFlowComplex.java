public final class ArrayFlowComplex {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        String[] arr = new String[2];
        arr[0] = source(); // Line 7
        arr[1] = "safe";
        
        sink(arr[0]); // Line 10
        sink(arr[1]); // Line 11
    }
}
