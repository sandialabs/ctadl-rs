//! Lazy line mapping: build line_starts from artifact bytes for offset → line/column.

/// Line map: byte offset of each line start. First element is always 0.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LineMap {
    pub line_starts: Vec<u32>,
}

impl LineMap {
    /// Builds a line map from file content. On each `\n`, records the start of the next line.
    pub fn from_bytes(content: &[u8]) -> Self {
        let mut line_starts = vec![0];
        for (i, &b) in content.iter().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self { line_starts }
    }

    /// Number of lines (line_starts.len()).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}
