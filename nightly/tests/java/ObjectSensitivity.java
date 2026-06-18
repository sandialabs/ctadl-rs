public final class ObjectSensitivity {
    String field;

    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        ObjectSensitivity obj1 = new ObjectSensitivity();
        ObjectSensitivity obj2 = new ObjectSensitivity();
        
        obj1.field = source(); // Line 11
        obj2.field = "safe";
        
        sink(obj1.field); // Line 14
        sink(obj2.field); // Line 15
    }
}
