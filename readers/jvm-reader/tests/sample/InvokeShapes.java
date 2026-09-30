interface ShapeOps {
    int apply(int a, int b);

    default int applyTwice(int a, int b) {
        return apply(a, b) + apply(a + 1, b + 1);
    }
}

public class InvokeShapes implements ShapeOps {
    private final int bias;

    public InvokeShapes(int bias) {
        this.bias = bias;
    }

    @Override
    public int apply(int a, int b) {
        return a * b + bias;
    }

    public static long longDoubleMix(long v, double d) {
        long x = v + 5L;
        double y = d * 1.5d;
        return x + (long) y;
    }

    public static String formatResult(int n) {
        return "n=" + n;
    }

    public static void main(String[] args) {
        ShapeOps ops = new InvokeShapes(3);
        int p = ops.apply(4, 6);
        int q = ops.applyTwice(2, 5);
        long r = longDoubleMix(10L, 2.25d);
        String s = formatResult(p + q + (int) r);
        System.out.println(s.toLowerCase());
    }
}
