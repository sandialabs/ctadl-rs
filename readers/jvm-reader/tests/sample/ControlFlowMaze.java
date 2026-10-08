public class ControlFlowMaze {
    public static int mergePaths(int x, int y) {
        int v;
        if (x > y) {
            v = x - y;
        } else if (x == y) {
            v = x * 3;
        } else {
            v = y - x;
        }

        int acc = 0;
        for (int i = 0; i < 4; i++) {
            if ((i & 1) == 0) {
                acc += v + i;
            } else {
                acc -= v - i;
            }
        }
        if ((acc & 1) == 0) {
            acc = (acc * 2) + 11;
        } else {
            acc = (acc / 2) - 7;
        }

        return acc;
    }

    public static int nestedLoopMath(int seed) {
        int total = seed;
        for (int i = 0; i < 3; i++) {
            for (int j = 0; j < 2; j++) {
                if (((i + j) & 1) == 0) {
                    total += (i * 7) + j;
                } else {
                    total -= (j * 5) - i;
                }
            }
        }
        return total;
    }

    public static void main(String[] args) {
        int a = mergePaths(9, 3);
        int b = mergePaths(2, 7);
        int c = nestedLoopMath(2);
        System.out.println(a + b + c);
    }
}
