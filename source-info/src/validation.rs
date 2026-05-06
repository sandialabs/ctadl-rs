//! Span validation: ensure start and start+len are within file bounds.

use crate::error::ValidationError;
use crate::ids::FileId;
use crate::span::SpanLen;

/// Validates that a span (start, len) is within file_size.
/// Returns Ok(()) or Err(ValidationError).
pub fn validate_span(
    file_id: FileId,
    start: u32,
    len: SpanLen,
    file_size: u32,
) -> Result<(), ValidationError> {
    if start > file_size {
        return Err(ValidationError::SpanStartOutOfBounds {
            file: file_id,
            start,
            file_size,
        });
    }
    let end = match len {
        SpanLen::Empty => start,
        SpanLen::ToLineEnd => {
            // ToLineEnd is valid as long as start is in bounds; end is computed at read time
            return Ok(());
        }
        SpanLen::ByteLen(n) => start.checked_add(n).ok_or(ValidationError::SpanOverflow {
            file: file_id,
            start,
            len: n,
        })?,
    };
    if end > file_size {
        return Err(ValidationError::SpanEndOutOfBounds {
            file: file_id,
            start,
            len: end - start,
            file_size,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::FileId;

    const FILE: FileId = FileId(1);

    #[test]
    fn validate_span_valid() {
        assert!(validate_span(FILE, 0, SpanLen::Empty, 100).is_ok());
        assert!(validate_span(FILE, 10, SpanLen::ByteLen(5), 20).is_ok());
        assert!(validate_span(FILE, 0, SpanLen::ToLineEnd, 50).is_ok());
    }

    #[test]
    fn validate_span_start_out_of_bounds() {
        let err = validate_span(FILE, 100, SpanLen::Empty, 50).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SpanStartOutOfBounds {
                start: 100,
                file_size: 50,
                ..
            }
        ));
    }

    #[test]
    fn validate_span_end_out_of_bounds() {
        let err = validate_span(FILE, 10, SpanLen::ByteLen(50), 20).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SpanEndOutOfBounds {
                start: 10,
                len: 50,
                file_size: 20,
                ..
            }
        ));
    }
}
