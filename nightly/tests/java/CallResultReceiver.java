// CallResultReceiver.java
public final class CallResultReceiver {
    String data;
    static CallResultReceiver holder = new CallResultReceiver();

    static CallResultReceiver holder() {
        return holder;
    }

    void set(String s) {
        this.data = s;
    }

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        holder().set(source());
        sink(holder.data);
    }
}
