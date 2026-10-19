// ConditionalReceiver.java
public final class ConditionalReceiver {
    String data;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        ConditionalReceiver a = new ConditionalReceiver();
        ConditionalReceiver b = new ConditionalReceiver();
        a.data = source();
        sink((args.length > 0 ? a : b).data);
    }
}
