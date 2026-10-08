/** Minimal array load/store for flow normalization tests. */
public class ArrayFlow {
    public static void touch(int[] a) {
        int x = a[0];
        a[1] = x;
    }
}
