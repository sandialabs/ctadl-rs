// CompoundArrayAssign.java
public final class CompoundArrayAssign {
    static long source() {
        return 7L;
    }

    static void sink(long v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        int[] a = new int[2];
        a[0] += (int) source();
        sink(a[0]);
        long[] la = new long[2];
        la[1] += source();
        sink(la[1]);
    }
}
