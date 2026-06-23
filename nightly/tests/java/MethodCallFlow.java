// MethodCallFlow.java
interface MyInterface {
    String process(String in);
}

class MyImpl implements MyInterface {
    public String process(String in) {
        return in + "_processed";
    }
}

public final class MethodCallFlow {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String data = source();
        MyInterface obj = new MyImpl();
        String result = obj.process(data);
        sink(result);
    }
}
