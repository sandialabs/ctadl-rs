// AnotherExample.java
public final class AnotherExample {
	
    public static void main(String[] args) {
        String tainted = source();
        String propagated = intermediate(tainted);
        sink(propagated);
    }
	
    // Shift line numbers
	
    // SOURCE: returns data that (pretend) comes from outside the program.
    static String source() {
        return System.getenv("USER"); // simple, available on standard Java
    }
	
    // INTERMEDIATE: transforms/propagates the data.
    static String intermediate(String in) {
        return in.toUpperCase();
    }
	
    // SINK: consumes the data in a way that could be sensitive.
    static void sink(String s) {
        System.out.println(s);
    }
}
