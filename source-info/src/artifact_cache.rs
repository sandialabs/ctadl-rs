//! Artifact cache: avoid repeated parsing by caching LineMaps per ArtifactId.

use std::collections::BTreeMap as HashMap;

use crate::ids::ArtifactId;
use crate::line_map::LineMap;

/// Caches line maps per artifact. Caller supplies content when computing.
#[derive(Debug)]
pub struct ArtifactCache {
    line_maps: HashMap<ArtifactId, LineMap>,
}

impl ArtifactCache {
    pub fn new() -> Self {
        Self {
            line_maps: HashMap::new(),
        }
    }

    /// Returns the line map for the artifact, computing it from `content` if not cached.
    pub fn get_or_compute(&mut self, artifact_id: ArtifactId, content: &[u8]) -> &LineMap {
        self.line_maps
            .entry(artifact_id)
            .or_insert_with(|| LineMap::from_bytes(content))
    }

    /// Returns the cached line map if present.
    pub fn get(&self, artifact_id: ArtifactId) -> Option<&LineMap> {
        self.line_maps.get(&artifact_id)
    }
}

impl Default for ArtifactCache {
    fn default() -> Self {
        Self::new()
    }
}
