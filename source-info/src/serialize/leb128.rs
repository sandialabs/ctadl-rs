//! ULEB128 and SLEB128 encoding/decoding for u32 and i32.

use std::io::{Read, Write};

/// Encodes `value` as ULEB128 into `buf`, returns number of bytes written.
pub fn write_uleb128(buf: &mut impl Write, value: u32) -> std::io::Result<usize> {
    let mut n = value;
    let mut count = 0;
    loop {
        let mut byte = (n & 0x7F) as u8;
        n >>= 7;
        if n != 0 {
            byte |= 0x80;
        }
        buf.write_all(&[byte])?;
        count += 1;
        if n == 0 {
            break;
        }
    }
    Ok(count)
}

/// Decodes ULEB128 from `r`, returns value and number of bytes read.
pub fn read_uleb128(r: &mut impl Read) -> std::io::Result<(u32, usize)> {
    let mut value: u32 = 0;
    let mut shift = 0;
    let mut count = 0;
    loop {
        let mut byte = [0u8; 1];
        r.read_exact(&mut byte)?;
        count += 1;
        value |= (byte[0] as u32 & 0x7F) << shift;
        if (byte[0] & 0x80) == 0 {
            break;
        }
        shift += 7;
        if shift >= 35 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ULEB128 overflow",
            ));
        }
    }
    Ok((value, count))
}

/// Encodes `value` as SLEB128 into `buf`, returns number of bytes written.
pub fn write_sleb128(buf: &mut impl Write, value: i32) -> std::io::Result<usize> {
    let mut n = value;
    let mut count = 0;
    loop {
        let mut byte = (n & 0x7F) as u8;
        n >>= 7;
        let more = !((n == 0 && (byte & 0x40) == 0) || (n == -1 && (byte & 0x40) != 0));
        if more {
            byte |= 0x80;
        }
        buf.write_all(&[byte])?;
        count += 1;
        if !more {
            break;
        }
    }
    Ok(count)
}

/// Decodes SLEB128 from `r`, returns value and number of bytes read.
pub fn read_sleb128(r: &mut impl Read) -> std::io::Result<(i32, usize)> {
    let mut value: i32 = 0;
    let mut shift = 0;
    let mut count = 0;
    let mut byte;
    loop {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        count += 1;
        byte = b[0];
        value |= ((byte & 0x7F) as i32) << shift;
        shift += 7;
        if (byte & 0x80) == 0 {
            break;
        }
        if shift >= 35 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SLEB128 overflow",
            ));
        }
    }
    if shift < 32 && (byte & 0x40) != 0 {
        value |= !0 << shift;
    }
    Ok((value, count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn uleb128_roundtrip() {
        for &v in &[0u32, 1, 127, 128, 16383, 16384, 0xFFFF, 0xFFFFFFFF] {
            let mut buf = Vec::new();
            write_uleb128(&mut buf, v).unwrap();
            let (dec, _) = read_uleb128(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(dec, v);
        }
    }

    #[test]
    fn sleb128_roundtrip() {
        for &v in &[0i32, 1, -1, 63, 64, -64, -65, 0x7FFF, -0x8000] {
            let mut buf = Vec::new();
            write_sleb128(&mut buf, v).unwrap();
            let (dec, _) = read_sleb128(&mut Cursor::new(&buf)).unwrap();
            assert_eq!(dec, v);
        }
    }
}
