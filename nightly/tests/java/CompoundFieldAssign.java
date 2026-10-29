// CompoundFieldAssign.java
public final class CompoundFieldAssign {
    int n;

    static int source() {
        return 7;
    }

    static void sink(int v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        CompoundFieldAssign c = new CompoundFieldAssign();
        c.n += source();
        sink(c.n);
    }
}
