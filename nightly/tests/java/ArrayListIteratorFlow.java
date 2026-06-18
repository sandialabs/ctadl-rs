import java.util.ArrayList;

class Person {
    String name;
    void setName(String name) { this.name = name; }
    String getName() { return this.name; }
}

public final class ArrayListIteratorFlow {
    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        ArrayList<Person> list = new ArrayList<>();
        Person p1 = new Person();
        String taint_source = source();
        // Line 17
        p1.setName(taint_source); // Line 18
        // Line 19
        list.add(p1);             // Line 20
        // Line 21
        for (Person p : list) {   // Line 22
            // Line 23
            String x = p.getName(); // Line 24
            // Line 25
            sink(x);                // Line 26
        }
    }
}
