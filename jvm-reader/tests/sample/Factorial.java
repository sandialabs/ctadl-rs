/**
 * Recursion and a counted loop. Revives the `Factorial` classfile-parser
 * fixture as source: exercises a self `invokestatic`, `imul`/`lmul`, an `i2l`
 * widening, and forward/back branches that none of the flat int samples cover.
 */
public class Factorial {
    static int recursive(int n) {
        if (n <= 1) {
            return 1;
        }
        return n * recursive(n - 1);
    }

    static long iterative(int n) {
        long acc = 1;
        for (int i = 2; i <= n; i++) {
            acc *= i;
        }
        return acc;
    }
}
