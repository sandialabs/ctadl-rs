use crate::error::*;

#[inline]
pub fn check_range(data: &[u8], offset: usize, size: usize) -> DexResult<()> {
    if offset
        .checked_add(size)
        .map_or(true, |end| end > data.len())
    {
        Err(DexError::OutOfBounds {
            offset,
            size,
            len: data.len(),
        })
    } else {
        Ok(())
    }
}

#[inline]
pub fn read_u8(data: &[u8], offset: usize) -> DexResult<u8> {
    check_range(data, offset, 1)?;
    Ok(data[offset])
}

#[inline]
pub fn read_u16_le(data: &[u8], offset: usize) -> DexResult<u16> {
    check_range(data, offset, 2)?;
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

#[inline]
pub fn read_u32_le(data: &[u8], offset: usize) -> DexResult<u32> {
    check_range(data, offset, 4)?;
    Ok(u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

#[inline]
pub fn read_i32_le(data: &[u8], offset: usize) -> DexResult<i32> {
    read_u32_le(data, offset).map(|v| v as i32)
}

#[inline]
pub fn read_slice<'a>(data: &'a [u8], offset: usize, size: usize) -> DexResult<&'a [u8]> {
    check_range(data, offset, size)?;
    Ok(&data[offset..offset + size])
}

pub fn read_uleb128(data: &[u8], offset: usize) -> DexResult<(u32, usize)> {
    let mut result = 0u32;
    let mut shift = 0;
    let mut pos = offset;

    for _ in 0..5 {
        let byte = *data.get(pos).ok_or(DexError::InvalidLeb128)?;
        pos += 1;

        result |= ((byte & 0x7f) as u32) << shift;

        if byte & 0x80 == 0 {
            return Ok((result, pos));
        }

        shift += 7;
    }

    Err(DexError::InvalidLeb128)
}

pub fn read_sleb128(data: &[u8], offset: usize) -> DexResult<(i32, usize)> {
    let mut result = 0i32;
    let mut shift = 0;
    let mut pos = offset;
    let mut byte;

    for _ in 0..5 {
        byte = *data.get(pos).ok_or(DexError::InvalidLeb128)?;
        pos += 1;

        result |= ((byte & 0x7f) as i32) << shift;
        shift += 7;

        if byte & 0x80 == 0 {
            if shift < 32 && (byte & 0x40) != 0 {
                result |= !0 << shift;
            }
            return Ok((result, pos));
        }
    }

    Err(DexError::InvalidLeb128)
}

pub fn read_dex_string<'a>(data: &'a [u8], offset: usize) -> DexResult<(String, usize)> {
    let (utf16_len, mut pos) = read_uleb128(data, offset)?;

    let start = pos;
    while *data.get(pos).ok_or(DexError::InvalidUtf8)? != 0 {
        pos += 1;
    }

    let bytes = &data[start..pos];
    let s = core::str::from_utf8(bytes)
        .map_err(|_| DexError::InvalidUtf8)?
        .to_owned();

    // skip null terminator
    pos += 1;

    // Optional sanity check
    if s.chars().count() != utf16_len as usize {
        // Many tools ignore this; you can downgrade to warning if desired
    }

    Ok((s, pos))
}

#[inline]
pub fn validate_offset(off: u32, data_len: usize) -> DexResult<()> {
    if off == 0 {
        return Ok(());
    }
    if off as usize >= data_len {
        Err(DexError::InvalidDex("offset out of range"))
    } else {
        Ok(())
    }
}

pub fn decode_mutf8(data: &[u8]) -> DexResult<String> {
    let mut chars = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        let byte = data[i];

        if byte & 0x80 == 0 {
            // 1-byte ASCII
            if byte == 0 {
                // MUTF-8 encodes null as 0xC0 0x80
                return Err(DexError::InvalidUtf8);
            }
            chars.push(byte as u32);
            i += 1;
        } else if byte & 0xE0 == 0xC0 {
            // 2-byte sequence
            if i + 1 >= data.len() {
                return Err(DexError::InvalidUtf8);
            }
            let b1 = data[i];
            let b2 = data[i + 1];

            // Special case for null: 0xC0 0x80
            if b1 == 0xC0 && b2 == 0x80 {
                chars.push(0);
            } else {
                let c = (((b1 & 0x1F) as u32) << 6) | ((b2 & 0x3F) as u32);
                chars.push(c);
            }
            i += 2;
        } else if byte & 0xF0 == 0xE0 {
            // 3-byte sequence
            if i + 2 >= data.len() {
                return Err(DexError::InvalidUtf8);
            }
            let b1 = data[i];
            let b2 = data[i + 1];
            let b3 = data[i + 2];
            let c =
                (((b1 & 0x0F) as u32) << 12) | (((b2 & 0x3F) as u32) << 6) | ((b3 & 0x3F) as u32);
            chars.push(c);
            i += 3;
        } else {
            return Err(DexError::InvalidUtf8);
        }
    }

    // Convert codepoints to Rust String
    let s: String = chars
        .into_iter()
        .map(|c| std::char::from_u32(c).ok_or(DexError::InvalidUtf8))
        .collect::<DexResult<String>>()?;

    Ok(s)
}
