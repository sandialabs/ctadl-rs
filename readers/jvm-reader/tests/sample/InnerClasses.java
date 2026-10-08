/**
 * A static nested class. Revives the `InnerClasses` fixture as source: compiles
 * to two `.class` files (so the JAR/multi-class paths see more than one entry)
 * carrying an InnerClasses attribute, and exercises object construction
 * (`new`/`dup`/`invokespecial`), `putfield`/`getfield`, and a final-field ctor.
 */
public class InnerClasses {
    static class Point {
        final int x;
        final int y;

        Point(int x, int y) {
            this.x = x;
            this.y = y;
        }

        int sum() {
            return x + y;
        }
    }

    static Point origin() {
        return new Point(0, 0);
    }

    int useNested() {
        return new Point(3, 4).sum();
    }
}
