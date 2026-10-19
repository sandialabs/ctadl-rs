// ConditionalReceiverStore.java
public final class ConditionalReceiverStore {
    String data;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        ConditionalReceiverStore a = new ConditionalReceiverStore();
        ConditionalReceiverStore b = new ConditionalReceiverStore();
        (args.length > 0 ? a : b).data = source();
        sink(a.data);
    }
}
