/**
 * Only well-formed supplementary characters: no unpaired surrogates at all.
 *
 * The class file encodes an emoji as a CESU-8 surrogate pair
 * (`ED A0 BD ED B8 80`), so a decoder that maps each three-byte sequence to a
 * scalar value independently rejects every class containing one -- an emoji, a
 * CJK extension character, any supplementary symbol in a literal -- not just
 * the deliberately unpaired case next door.
 */
public class PairedOnly {
    static final String EMOJI = "😀";
    public static int len() { return EMOJI.length(); }
}
