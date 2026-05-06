//! Offset to line/column: given a byte offset and LineMap, compute (line, column).
//!
//! Column is in the same units as the artifact encoding (UTF-8 or UTF-16 code units).

use crate::line_map::LineMap;

/// 1-based line and column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LineColumn {
    pub line: u32,
    pub column: u32,
}

/// Returns (1-based line, 0-based column in bytes) for the given byte offset.
/// Uses partition_point to find the line such that line_starts[line] <= offset.
pub fn offset_to_line_column(line_map: &LineMap, offset: u32) -> LineColumn {
    let line = line_map
        .line_starts
        .partition_point(|&s| s <= offset)
        .saturating_sub(1);
    let line_start = line_map.line_starts.get(line).copied().unwrap_or(0);
    let column = offset.saturating_sub(line_start);
    LineColumn {
        line: (line + 1) as u32,
        column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line_map::LineMap;

    #[test]
    fn line_map_from_bytes() {
        let lm = LineMap::from_bytes(b"a\nbb\nccc\n");
        assert_eq!(lm.line_starts, [0, 2, 5, 9]);
        assert_eq!(lm.line_count(), 4);
    }

    #[test]
    fn offset_to_line_column_semantics() {
        let lm = LineMap::from_bytes(b"a\nbb\nccc\n");
        assert_eq!(
            offset_to_line_column(&lm, 0),
            LineColumn { line: 1, column: 0 }
        );
        assert_eq!(
            offset_to_line_column(&lm, 2),
            LineColumn { line: 2, column: 0 }
        );
        assert_eq!(
            offset_to_line_column(&lm, 5),
            LineColumn { line: 3, column: 0 }
        );
        assert_eq!(
            offset_to_line_column(&lm, 4),
            LineColumn { line: 2, column: 2 }
        );
    }
}
