// Dup2X2LongArrayChainFlow.java
public final class Dup2X2LongArrayChainFlow {
    static long source() {
        return 7L;
    }

    static void sink(long v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        long[] a = new long[2];
        long x = a[0] = source();
        sink(x);
    }
}
