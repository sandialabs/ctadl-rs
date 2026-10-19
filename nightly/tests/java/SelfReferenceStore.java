// SelfReferenceStore.java
public final class SelfReferenceStore {
    String data;
    SelfReferenceStore self;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    void register() {
        this.self = this;
    }

    public static void main(String[] args) {
        SelfReferenceStore n = new SelfReferenceStore();
        n.data = source();
        n.register();
        sink(n.self.data);
    }
}
