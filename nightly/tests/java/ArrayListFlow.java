import java.util.ArrayList;

public final class ArrayListFlow {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        ArrayList<String> list = new ArrayList<>();
        list.add(source()); // Line 9
        String data = list.get(0); // Line 10
        sink(data); // Line 11
    }
}
