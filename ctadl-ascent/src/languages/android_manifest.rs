use std::path::Path;

use dex_reader::axml::{AndroidManifest, ManifestNode, ManifestNodeAttr, ManifestNodeChild};

use crate::error::Error;
use crate::project::ArtifactImport;

pub fn save_manifest<P: AsRef<Path>>(manifest: &AndroidManifest, path: P) -> Result<(), Error> {
    use crate::facts::schema::{manifest_node, manifest_node_attr, manifest_node_child};
    let dir = path.as_ref();
    manifest_node::try_save(
        dir,
        manifest
            .nodes
            .iter()
            .map(|node| (node.id, node.tag.clone())),
    )?;
    manifest_node_child::try_save(
        dir,
        manifest
            .children
            .iter()
            .map(|child| (child.parent_id, child.child_id)),
    )?;
    manifest_node_attr::try_save(
        dir,
        manifest
            .attrs
            .iter()
            .map(|attr| (attr.node_id, attr.key.clone(), attr.value.clone())),
    )?;
    Ok(())
}

pub fn load_manifest<P: AsRef<Path>>(path: P) -> Result<AndroidManifest, Error> {
    use crate::facts::schema::{manifest_node, manifest_node_attr, manifest_node_child};
    let dir = path.as_ref();
    Ok(AndroidManifest {
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

pub fn import_from_apk(import: &ArtifactImport) -> Result<Option<AndroidManifest>, Error> {
    let bytes =
        match dex_reader::apk::read_bundle_entry(&import.artifact_path, "AndroidManifest.xml") {
            Ok(bytes) => bytes,
            Err(e) => {
                log::debug!("{}: no AndroidManifest.xml imported: {e}", import.name);
                return Ok(None);
            }
        };
    let manifest = dex_reader::axml::parse_manifest(&bytes)?;
    save_manifest(&manifest, import.import_path())?;
    Ok(Some(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dex_reader::axml::parse_manifest;

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
        save_manifest(&manifest, dir.path()).unwrap();

        let loaded = load_manifest(dir.path()).unwrap();
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
    fn decoding_error_renders_with_engine_prefix() {
        let err: Error = parse_manifest(&0x0008_0003u32.to_le_bytes())
            .unwrap_err()
            .into();
        let text = err.to_string();
        assert!(
            text.starts_with("android manifest error: unexpected end of manifest"),
            "{text}"
        );
    }
}
