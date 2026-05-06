use crate::error::*;

#[inline]
pub fn check_range(data: &[u8], offset: usize, size: usize) -> ClassFileResult<()> {
    if offset
        .checked_add(size)
        .map_or(true, |end| end > data.len())
    {
        Err(ClassFileError::OutOfBounds {
            offset,
            size,
            len: data.len(),
        })
    } else {
        Ok(())
    }
}

#[inline]
pub fn read_u8(data: &[u8], offset: usize) -> ClassFileResult<u8> {
    check_range(data, offset, 1)?;
    Ok(data[offset])
}

#[inline]
pub fn read_u16_be(data: &[u8], offset: usize) -> ClassFileResult<u16> {
    check_range(data, offset, 2)?;
    Ok(u16::from_be_bytes([data[offset], data[offset + 1]]))
}

#[inline]
pub fn read_u32_be(data: &[u8], offset: usize) -> ClassFileResult<u32> {
    check_range(data, offset, 4)?;
    Ok(u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

#[inline]
pub fn read_i32_be(data: &[u8], offset: usize) -> ClassFileResult<i32> {
    read_u32_be(data, offset).map(|v| v as i32)
}

#[inline]
pub fn read_slice<'a>(data: &'a [u8], offset: usize, size: usize) -> ClassFileResult<&'a [u8]> {
    check_range(data, offset, size)?;
    Ok(&data[offset..offset + size])
}

#[inline]
pub fn validate_offset(off: u32, data_len: usize) -> ClassFileResult<()> {
    if off == 0 {
        return Ok(());
    }
    if off as usize >= data_len {
        Err(ClassFileError::InvalidClassFile("offset out of range"))
    } else {
        Ok(())
    }
}

/// Decode JVM modified UTF-8 (length-prefixed bytes in Utf8_info).
/// Same as DEX MUTF-8: null is 0xC0 0x80, no raw 0x00 in stream.
pub fn decode_modified_utf8(data: &[u8]) -> ClassFileResult<String> {
    let mut chars = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        let byte = data[i];

        if byte & 0x80 == 0 {
            if byte == 0 {
                return Err(ClassFileError::InvalidUtf8);
            }
            chars.push(byte as u32);
            i += 1;
        } else if byte & 0xE0 == 0xC0 {
            if i + 1 >= data.len() {
                return Err(ClassFileError::InvalidUtf8);
            }
            let b1 = data[i];
            let b2 = data[i + 1];
            if b1 == 0xC0 && b2 == 0x80 {
                chars.push(0);
            } else {
                let c = (((b1 & 0x1F) as u32) << 6) | ((b2 & 0x3F) as u32);
                chars.push(c);
            }
            i += 2;
        } else if byte & 0xF0 == 0xE0 {
            if i + 2 >= data.len() {
                return Err(ClassFileError::InvalidUtf8);
            }
            let b1 = data[i];
            let b2 = data[i + 1];
            let b3 = data[i + 2];
            let c =
                (((b1 & 0x0F) as u32) << 12) | (((b2 & 0x3F) as u32) << 6) | ((b3 & 0x3F) as u32);
            chars.push(c);
            i += 3;
        } else {
            return Err(ClassFileError::InvalidUtf8);
        }
    }

    let s: String = chars
        .into_iter()
        .map(|c| std::char::from_u32(c).ok_or(ClassFileError::InvalidUtf8))
        .collect::<ClassFileResult<String>>()?;

    Ok(s)
}
