// CallResultFieldLoad.java
public final class CallResultFieldLoad {
    String data;

    static CallResultFieldLoad fresh() {
        return new CallResultFieldLoad();
    }

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        CallResultFieldLoad a = new CallResultFieldLoad();
        a.data = source();
        System.out.println(a);
        sink(fresh().data);
    }
}
