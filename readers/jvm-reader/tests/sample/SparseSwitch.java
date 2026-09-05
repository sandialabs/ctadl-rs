/**
 * A sparse integer switch, which javac lowers to `lookupswitch`, with a
 * back-edge join.
 *
 * Mirrors brut.androlib.res.decoder.BinaryResourceParser.parseTable: a
 * while-loop whose body switches on a freshly computed int and whose default
 * arm branches back to the loop header. The case values are sparse, so javac
 * emits lookupswitch.
 *
 * Correct stack at the loop header is 0. The selector pushed before the
 * lookupswitch must be consumed by it; if the decoder gives lookupswitch a
 * zero stack effect the selector survives as a phantom slot and the default
 * arm re-enters the header with height 1.
 */
public class SparseSwitch {

    private int cursor;

    private int chunkType() {
        return cursor * 7919;
    }

    private void skipUnexpectedChunk(Object sink) {
        cursor++;
    }

    public void parseTable(Object sink) {
        while (cursor < 100) {          // loop header: correct stack = 0
            switch (chunkType()) {      // selector pushed, lookupswitch consumes it
                case 0:
                    cursor += 1;
                    break;
                case 4097:
                    cursor += 2;
                    break;
                case 131073:
                    cursor += 3;
                    break;
                case 16777216:
                    cursor += 4;
                    break;
                default:                // back-edge to the loop header
                    skipUnexpectedChunk(sink);
                    break;
            }
        }
    }
}
