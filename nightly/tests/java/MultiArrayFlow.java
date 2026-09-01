// MultiArrayFlow.java
//
// Taint carried through a multi-dimensional array. `multianewarray` pops one
// int count per dimension and pushes the array; leaving it unmodelled makes
// every `new T[a][b]` leave its counts behind as phantom stack slots, which
// surface at the next join.
public final class MultiArrayFlow {

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    /** `multianewarray` with two dimensions, then a store and a load. */
    static String throughGrid(String in, int rows, int cols, boolean flag) {
        String[][] grid = new String[rows][cols];
        grid[0][0] = in;
        if (flag) {
            grid[0][0] = in;
        }
        return grid[0][0];
    }

    public static void main(String[] args) {
        sink(throughGrid(source(), 2, 2, true));
    }
}
