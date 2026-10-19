// ArrayIndexAsValue.java
public final class ArrayIndexAsValue {
    static int source() {
        return 1;
    }

    static void sink(int v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        int[] a = new int[4];
        int i = source();
        a[i] = i;
        sink(a[0]);
    }
}
