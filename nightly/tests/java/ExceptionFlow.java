// ExceptionFlow.java
public final class ExceptionFlow {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        try {
            String data = source();
            throw new Exception(data);
        } catch (Exception e) {
            sink(e.getMessage());
        }
    }
}
