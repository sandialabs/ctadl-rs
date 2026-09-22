use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::project::ArtifactImport;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestNode {
    pub id: u32,
    pub tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestNodeChild {
    pub parent_id: u32,
    pub child_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestNodeAttr {
    pub node_id: u32,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AndroidManifest {
    pub nodes: Vec<ManifestNode>,
    pub children: Vec<ManifestNodeChild>,
    pub attrs: Vec<ManifestNodeAttr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidComponent {
    pub node_id: u32,
    pub tag: String,
    pub name: String,
    pub descriptor: Option<String>,
    pub exported: Option<bool>,
    pub enabled: Option<bool>,
    pub permission: Option<String>,
    pub target_activity: Option<String>,
    pub has_intent_filter: bool,
}

impl AndroidManifest {
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        use crate::facts::schema::{manifest_node, manifest_node_attr, manifest_node_child};
        let dir = path.as_ref();
        manifest_node::try_save(
            dir,
            self.nodes.iter().map(|node| (node.id, node.tag.clone())),
        )?;
        manifest_node_child::try_save(
            dir,
            self.children
                .iter()
                .map(|child| (child.parent_id, child.child_id)),
        )?;
        manifest_node_attr::try_save(
            dir,
            self.attrs
                .iter()
                .map(|attr| (attr.node_id, attr.key.clone(), attr.value.clone())),
        )?;
        Ok(())
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        use crate::facts::schema::{manifest_node, manifest_node_attr, manifest_node_child};
        let dir = path.as_ref();
        Ok(Self {
            nodes: manifest_node::try_load(dir)?
                .into_iter()
                .map(|(id, tag)| ManifestNode { id, tag })
                .collect(),
            children: manifest_node_child::try_load(dir)?
                .into_iter()
                .map(|(parent_id, child_id)| ManifestNodeChild {
                    parent_id,
                    child_id,
                })
                .collect(),
            attrs: manifest_node_attr::try_load(dir)?
                .into_iter()
                .map(|(node_id, key, value)| ManifestNodeAttr {
                    node_id,
                    key,
                    value,
                })
                .collect(),
        })
    }

    pub fn package(&self) -> Option<&str> {
        self.attrs
            .iter()
            .find(|attr| attr.node_id == 0 && attr.key == "package")
            .map(|attr| attr.value.as_str())
    }

    pub fn components(&self) -> Vec<AndroidComponent> {
        let package = self.package().unwrap_or_default();
        self.nodes
            .iter()
            .filter(|node| is_component_tag(&node.tag))
            .map(|node| {
                let name = self.attr(node.id, "name").unwrap_or_default().to_string();
                let descriptor = normalize_component_name(package, &name);
                let target_activity = self
                    .attr(node.id, "targetActivity")
                    .and_then(|target| normalize_component_name(package, target));
                AndroidComponent {
                    node_id: node.id,
                    tag: node.tag.clone(),
                    name,
                    descriptor,
                    exported: self.attr(node.id, "exported").and_then(parse_bool),
                    enabled: self.attr(node.id, "enabled").and_then(parse_bool),
                    permission: self.attr(node.id, "permission").map(str::to_string),
                    target_activity,
                    has_intent_filter: self.has_child_tag(node.id, "intent-filter"),
                }
            })
            .collect()
    }

    fn attr(&self, node_id: u32, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|attr| attr.node_id == node_id && attr_key_matches(&attr.key, name))
            .map(|attr| attr.value.as_str())
    }

    fn has_child_tag(&self, node_id: u32, tag: &str) -> bool {
        self.children.iter().any(|child| {
            child.parent_id == node_id
                && self
                    .nodes
                    .get(child.child_id as usize)
                    .is_some_and(|node| node.tag == tag)
        })
    }
}

pub fn normalize_component_name(package: &str, name: &str) -> Option<String> {
    if name.is_empty() || package.is_empty() {
        return None;
    }
    let dotted = if name.starts_with('.') {
        format!("{package}{name}")
    } else if name.contains('.') {
        name.to_string()
    } else {
        format!("{package}.{name}")
    };
    Some(format!("L{};", dotted.replace('.', "/")))
}

fn is_component_tag(tag: &str) -> bool {
    matches!(
        tag,
        "activity" | "activity-alias" | "receiver" | "service" | "provider"
    )
}

fn attr_key_matches(key: &str, name: &str) -> bool {
    key == name || key == format!("android:{name}") || key.ends_with(&format!(":{name}"))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

pub fn parse_manifest(bytes: &[u8]) -> Result<AndroidManifest, Error> {
    if is_binary_xml(bytes) {
        return parse_binary_manifest(bytes);
    }
    parse_text_manifest(bytes)
}

pub fn import_from_apk(import: &ArtifactImport) -> Result<Option<AndroidManifest>, Error> {
    let bytes = match dex_reader::apk::read_apk_entry(&import.artifact_path, "AndroidManifest.xml")
    {
        Ok(bytes) => bytes,
        Err(e) => {
            log::debug!("{}: no AndroidManifest.xml imported: {e}", import.name);
            return Ok(None);
        }
    };
    let manifest = parse_manifest(&bytes)?;
    manifest.save(import.import_path())?;
    Ok(Some(manifest))
}

fn is_binary_xml(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == 0x0008_0003
}

fn parse_binary_manifest(bytes: &[u8]) -> Result<AndroidManifest, Error> {
    let mut pos = 0usize;
    let root = read_chunk_header(bytes, pos)?;
    if root.ty != 0x0003 {
        return Err(Error::AndroidManifest {
            message: format!("expected XML chunk 0x0003, found 0x{:04x}", root.ty),
        });
    }
    pos += root.header_size as usize;

    let mut strings = Vec::new();
    let mut stack = Vec::<u32>::new();
    let mut out = AndroidManifest::default();
    while pos + 8 <= bytes.len() && pos < root.size as usize {
        let chunk = read_chunk_header(bytes, pos)?;
        match chunk.ty {
            0x0001 => strings = parse_string_pool(bytes, pos, chunk)?,
            0x0102 => parse_start_element(bytes, pos, chunk, &strings, &mut stack, &mut out)?,
            0x0103 => {
                stack.pop();
            }
            _ => {}
        }
        if chunk.size == 0 {
            return Err(Error::AndroidManifest {
                message: format!("zero-sized binary XML chunk at offset {pos}"),
            });
        }
        pos = pos.saturating_add(chunk.size as usize);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct ChunkHeader {
    ty: u16,
    header_size: u16,
    size: u32,
}

fn parse_start_element(
    bytes: &[u8],
    offset: usize,
    chunk: ChunkHeader,
    strings: &[String],
    stack: &mut Vec<u32>,
    out: &mut AndroidManifest,
) -> Result<(), Error> {
    if chunk.size < 36 {
        return Err(Error::AndroidManifest {
            message: format!("start-element chunk too small at offset {offset}"),
        });
    }
    let name_idx = read_u32(bytes, offset + 20)?;
    let attr_start = read_u16(bytes, offset + 24)? as usize;
    let attr_size = read_u16(bytes, offset + 26)? as usize;
    let attr_count = read_u16(bytes, offset + 28)? as usize;

    let id = out.nodes.len() as u32;
    let tag = string_at(strings, name_idx).unwrap_or_else(|| format!("<string#{name_idx}>"));
    out.nodes.push(ManifestNode { id, tag });
    if let Some(parent_id) = stack.last().copied() {
        out.children.push(ManifestNodeChild {
            parent_id,
            child_id: id,
        });
    }

    let attrs_offset = offset + 16 + attr_start;
    for i in 0..attr_count {
        let attr_offset = attrs_offset + i * attr_size;
        if attr_offset + 20 > offset + chunk.size as usize || attr_offset + 20 > bytes.len() {
            return Err(Error::AndroidManifest {
                message: format!("attribute {i} exceeds start-element chunk at offset {offset}"),
            });
        }
        let ns_idx = read_u32(bytes, attr_offset)?;
        let name_idx = read_u32(bytes, attr_offset + 4)?;
        let raw_idx = read_u32(bytes, attr_offset + 8)?;
        let data_type = read_u8(bytes, attr_offset + 15)?;
        let data = read_u32(bytes, attr_offset + 16)?;

        let name = string_at(strings, name_idx).unwrap_or_else(|| format!("attr#{name_idx}"));
        let key = match string_at(strings, ns_idx) {
            Some(ns) if ns == "http://schemas.android.com/apk/res/android" => {
                format!("android:{name}")
            }
            Some(ns) => format!("{ns}:{name}"),
            None => name,
        };
        let value = if raw_idx != u32::MAX {
            string_at(strings, raw_idx).unwrap_or_else(|| format!("<string#{raw_idx}>"))
        } else {
            typed_value(data_type, data, strings)
        };
        out.attrs.push(ManifestNodeAttr {
            node_id: id,
            key,
            value,
        });
    }
    stack.push(id);
    Ok(())
}

fn typed_value(data_type: u8, data: u32, strings: &[String]) -> String {
    match data_type {
        0x01 => format!("@0x{data:08x}"),
        0x03 => string_at(strings, data).unwrap_or_else(|| format!("<string#{data}>")),
        0x10 => (data as i32).to_string(),
        0x11 => format!("0x{data:x}"),
        0x12 => {
            if data == 0 {
                "false".to_string()
            } else {
                "true".to_string()
            }
        }
        _ => format!("type=0x{data_type:02x}:0x{data:x}"),
    }
}

fn parse_string_pool(
    bytes: &[u8],
    offset: usize,
    chunk: ChunkHeader,
) -> Result<Vec<String>, Error> {
    if chunk.header_size < 28 {
        return Err(Error::AndroidManifest {
            message: format!("string-pool header too small at offset {offset}"),
        });
    }
    let string_count = read_u32(bytes, offset + 8)? as usize;
    let flags = read_u32(bytes, offset + 16)?;
    let strings_start = read_u32(bytes, offset + 20)? as usize;
    let utf8 = flags & 0x0000_0100 != 0;
    let offsets_start = offset + chunk.header_size as usize;
    let strings_base = offset + strings_start;
    let chunk_end = offset + chunk.size as usize;
    let mut strings = Vec::with_capacity(string_count);
    for i in 0..string_count {
        let off = read_u32(bytes, offsets_start + i * 4)? as usize;
        let string_offset = strings_base + off;
        if string_offset >= chunk_end || string_offset >= bytes.len() {
            return Err(Error::AndroidManifest {
                message: format!("string {i} points outside string pool"),
            });
        }
        strings.push(if utf8 {
            parse_utf8_string(bytes, string_offset, chunk_end)?
        } else {
            parse_utf16_string(bytes, string_offset, chunk_end)?
        });
    }
    Ok(strings)
}

fn parse_utf8_string(bytes: &[u8], offset: usize, limit: usize) -> Result<String, Error> {
    let (_, pos) = read_length8(bytes, offset, limit)?;
    let (byte_len, pos) = read_length8(bytes, pos, limit)?;
    if pos + byte_len > limit || pos + byte_len > bytes.len() {
        return Err(Error::AndroidManifest {
            message: "UTF-8 string exceeds string pool".to_string(),
        });
    }
    String::from_utf8(bytes[pos..pos + byte_len].to_vec()).map_err(|e| Error::AndroidManifest {
        message: format!("invalid UTF-8 string in string pool: {e}"),
    })
}

fn parse_utf16_string(bytes: &[u8], offset: usize, limit: usize) -> Result<String, Error> {
    let (unit_len, mut pos) = read_length16(bytes, offset, limit)?;
    if pos + unit_len * 2 > limit || pos + unit_len * 2 > bytes.len() {
        return Err(Error::AndroidManifest {
            message: "UTF-16 string exceeds string pool".to_string(),
        });
    }
    let mut units = Vec::with_capacity(unit_len);
    for _ in 0..unit_len {
        units.push(read_u16(bytes, pos)?);
        pos += 2;
    }
    String::from_utf16(&units).map_err(|e| Error::AndroidManifest {
        message: format!("invalid UTF-16 string in string pool: {e}"),
    })
}

fn read_length8(bytes: &[u8], offset: usize, limit: usize) -> Result<(usize, usize), Error> {
    let first = *bytes.get(offset).ok_or_else(|| Error::AndroidManifest {
        message: "truncated UTF-8 string length".to_string(),
    })?;
    if first & 0x80 == 0 {
        Ok((first as usize, offset + 1))
    } else {
        let second = *bytes
            .get(offset + 1)
            .filter(|_| offset + 1 < limit)
            .ok_or_else(|| Error::AndroidManifest {
                message: "truncated UTF-8 string length".to_string(),
            })?;
        Ok((
            (((first & 0x7f) as usize) << 8) | second as usize,
            offset + 2,
        ))
    }
}

fn read_length16(bytes: &[u8], offset: usize, limit: usize) -> Result<(usize, usize), Error> {
    let first = read_u16(bytes, offset)?;
    if first & 0x8000 == 0 {
        Ok((first as usize, offset + 2))
    } else {
        if offset + 4 > limit {
            return Err(Error::AndroidManifest {
                message: "truncated UTF-16 string length".to_string(),
            });
        }
        let second = read_u16(bytes, offset + 2)?;
        Ok((
            (((first & 0x7fff) as usize) << 16) | second as usize,
            offset + 4,
        ))
    }
}

fn string_at(strings: &[String], idx: u32) -> Option<String> {
    if idx == u32::MAX {
        None
    } else {
        strings.get(idx as usize).cloned()
    }
}

fn read_chunk_header(bytes: &[u8], offset: usize) -> Result<ChunkHeader, Error> {
    Ok(ChunkHeader {
        ty: read_u16(bytes, offset)?,
        header_size: read_u16(bytes, offset + 2)?,
        size: read_u32(bytes, offset + 4)?,
    })
}

fn read_u8(bytes: &[u8], offset: usize) -> Result<u8, Error> {
    bytes
        .get(offset)
        .copied()
        .ok_or_else(|| Error::AndroidManifest {
            message: format!("unexpected end of manifest at offset {offset}"),
        })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| Error::AndroidManifest {
            message: format!("unexpected end of manifest at offset {offset}"),
        })?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| Error::AndroidManifest {
            message: format!("unexpected end of manifest at offset {offset}"),
        })?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

fn parse_text_manifest(bytes: &[u8]) -> Result<AndroidManifest, Error> {
    let text = std::str::from_utf8(bytes).map_err(|e| Error::AndroidManifest {
        message: format!("manifest is not UTF-8 text XML: {e}"),
    })?;
    let doc = roxmltree::Document::parse(text).map_err(|e| Error::AndroidManifest {
        message: format!("manifest text XML parse failed: {e}"),
    })?;

    let mut out = AndroidManifest::default();
    append_element(doc.root_element(), None, &mut out);
    Ok(out)
}

fn append_element(node: roxmltree::Node<'_, '_>, parent: Option<u32>, out: &mut AndroidManifest) {
    let id = out.nodes.len() as u32;
    out.nodes.push(ManifestNode {
        id,
        tag: node.tag_name().name().to_string(),
    });
    if let Some(parent_id) = parent {
        out.children.push(ManifestNodeChild {
            parent_id,
            child_id: id,
        });
    }
    for attr in node.attributes() {
        let key = if let Some(ns) = attr.namespace() {
            format!("{ns}:{}", attr.name())
        } else {
            attr.name().to_string()
        };
        out.attrs.push(ManifestNodeAttr {
            node_id: id,
            key,
            value: attr.value().to_string(),
        });
    }
    for child in node.children().filter(|child| child.is_element()) {
        append_element(child, Some(id), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_manifest_becomes_triples() {
        let manifest = parse_manifest(
            br#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
                <application>
                    <activity android:name=".MainActivity" android:exported="true" />
                </application>
            </manifest>"#,
        )
        .unwrap();

        assert_eq!(manifest.nodes.len(), 3);
        assert!(manifest.nodes.iter().any(|n| n.tag == "manifest"));
        assert!(manifest.nodes.iter().any(|n| n.tag == "activity"));
        assert!(manifest.children.contains(&ManifestNodeChild {
            parent_id: 0,
            child_id: 1,
        }));
        assert!(
            manifest
                .attrs
                .iter()
                .any(|a| a.key == "package" && a.value == "com.example")
        );
        assert!(
            manifest
                .attrs
                .iter()
                .any(|a| a.key.ends_with(":name") && a.value == ".MainActivity")
        );
    }

    #[test]
    fn component_names_normalize_to_descriptors() {
        assert_eq!(
            normalize_component_name("com.example", ".MainActivity"),
            Some("Lcom/example/MainActivity;".to_string())
        );
        assert_eq!(
            normalize_component_name("com.example", "com.other.MainActivity"),
            Some("Lcom/other/MainActivity;".to_string())
        );
        assert_eq!(
            normalize_component_name("com.example", "MainActivity"),
            Some("Lcom/example/MainActivity;".to_string())
        );
    }

    #[test]
    fn manifest_round_trips_through_parquet_tables() {
        let manifest = parse_manifest(
            br#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example">
                <application>
                    <receiver android:name="Receiver" android:enabled="false" />
                </application>
            </manifest>"#,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        manifest.save(dir.path()).unwrap();

        let loaded = AndroidManifest::load(dir.path()).unwrap();
        assert_eq!(loaded, manifest);
        let components = loaded.components();
        assert_eq!(components.len(), 1);
        assert_eq!(
            components[0].descriptor.as_deref(),
            Some("Lcom/example/Receiver;")
        );
        assert_eq!(components[0].enabled, Some(false));
    }

    #[test]
    fn binary_xml_is_reported_clearly() {
        let err = parse_manifest(&0x0008_0003u32.to_le_bytes()).unwrap_err();
        assert!(err.to_string().contains("unexpected end of manifest"));
    }

    #[test]
    fn noto_binary_manifest_has_known_counts() {
        let apk = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("xtask/tests/dex/com.noto_54.apk");
        if !apk.exists() {
            return;
        }
        let bytes = dex_reader::apk::read_apk_entry(&apk, "AndroidManifest.xml").unwrap();
        let manifest = parse_manifest(&bytes).unwrap();
        assert_eq!(manifest.nodes.len(), 180);
        assert_eq!(manifest.attrs.len(), 302);
        assert_eq!(manifest.components().len(), 35);
        assert_eq!(manifest.package(), Some("com.noto"));
        assert!(manifest.components().iter().any(|component| {
            component.tag == "activity"
                && component.descriptor.as_deref() == Some("Lcom/noto/app/AppActivity;")
                && component.exported == Some(true)
                && component.has_intent_filter
        }));
        assert_eq!(
            manifest
                .components()
                .iter()
                .filter(|component| component.tag == "activity-alias")
                .count(),
            10
        );
    }
}
