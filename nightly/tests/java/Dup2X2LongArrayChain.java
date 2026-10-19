// Dup2X2LongArrayChain.java
public final class Dup2X2LongArrayChain {
    static int source() {
        return 1;
    }

    static void sink(long v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        long[] a = new long[2];
        int i = source();
        long y = System.nanoTime();
        long x = a[i] = y;
        sink(x);
    }
}
