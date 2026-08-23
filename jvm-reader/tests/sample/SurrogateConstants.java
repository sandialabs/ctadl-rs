/**
 * CONSTANT_Utf8 entries holding UTF-16 surrogate code units: a well-formed
 * pair, a lone high surrogate, a lone low surrogate, and a packed table mixing
 * them.
 *
 * The JVM stores strings as UTF-16 code units in modified UTF-8; a lone
 * surrogate is encoded as a plain three-byte sequence (0xED 0xA0 0x80 for
 * U+D800) and is perfectly legal there. Rust's char cannot represent one, so
 * char::from_u32 rejects it.
 *
 * This is exactly what packed lexer tables such as
 * com/android/tools/smali/smali/smaliFlexLexer do: they abuse String constants
 * as arbitrary UTF-16 code-unit arrays.
 *
 * The source is deliberately kept ASCII; every non-ASCII code unit below is
 * written as a unicode escape, so nothing depends on this file's own encoding.
 */
public class SurrogateConstants {

    /** A well-formed surrogate pair: one supplementary character, U+10000. */
    static final String PAIRED = "𐀀";

    /** A lone high surrogate: not a Unicode scalar value. */
    static final String UNPAIRED_HIGH = "\uD800";

    /** A lone low surrogate. */
    static final String UNPAIRED_LOW = "\uDC00";

    /**
     * A packed table in the style of a generated lexer: a run of code units
     * that mixes a well-formed pair with deliberately unpaired surrogates.
     */
    static final String PACKED_TABLE = " 𐀀\uD801􏿿\uDC02";

    public static int tableLength() {
        return PACKED_TABLE.length()
            + PAIRED.length()
            + UNPAIRED_HIGH.length()
            + UNPAIRED_LOW.length();
    }
}
