/**
 * A dense integer switch, which javac lowers to `tableswitch`, with a back-edge
 * join.
 *
 * Same shape as SparseSwitch, but with dense case values so javac emits
 * tableswitch instead of lookupswitch.
 */
public class DenseSwitch {

    private int cursor;

    private int chunkType() {
        return cursor & 7;
    }

    private void skipUnexpectedChunk(Object sink) {
        cursor++;
    }

    public void parseTable(Object sink) {
        while (cursor < 100) {          // loop header: correct stack = 0
            switch (chunkType()) {      // selector pushed, tableswitch consumes it
                case 0:
                    cursor += 1;
                    break;
                case 1:
                    cursor += 2;
                    break;
                case 2:
                    cursor += 3;
                    break;
                case 3:
                    cursor += 4;
                    break;
                case 4:
                    cursor += 5;
                    break;
                default:                // back-edge to the loop header
                    skipUnexpectedChunk(sink);
                    break;
            }
        }
    }
}
