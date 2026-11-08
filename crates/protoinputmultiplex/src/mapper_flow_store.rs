//! Bounded, versioned storage for named virtual-input mapper flows.
use anyhow::{Context, Result, ensure};
use mux_core::mapper::{Definition, MAX_JSON_BYTES};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const FLOW_VERSION: u32 = 1;
const MAX_NAME_BYTES: usize = 64;
const MAX_FLOW_BYTES: usize = MAX_JSON_BYTES + 1024;
const MAX_LIST_ENTRIES: usize = 4096;
pub(crate) const MAX_BROWSE_ENTRIES: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreviewNode {
    pub id: String,
    pub kind: String,
    pub position: [f64; 2],
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Preview {
    pub nodes: Vec<PreviewNode>,
    pub edges: Vec<[usize; 2]>,
    pub node_count: usize,
    pub edge_count: usize,
    pub group_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BrowseEntry {
    pub name: String,
    pub preview: Option<Preview>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BrowsePage {
    pub query: String,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub entries: Vec<BrowseEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Flow {
    pub version: u32,
    pub name: String,
    pub mapper: Definition,
}

impl Flow {
    pub(crate) fn new(name: String, mapper: Definition) -> Self {
        Self {
            version: FLOW_VERSION,
            name,
            mapper,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Store {
    directory: PathBuf,
}

impl Store {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            directory: data_root.join("mapper-flows"),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<String>> {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && is_plain(&metadata),
                "flow library must be a directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        let entries = fs::read_dir(&self.directory)?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file()
                || path.extension().and_then(|x| x.to_str()) != Some("json")
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            let Some(name) = decode_name(stem) else {
                continue;
            };
            if validate_name(&name).is_ok() {
                ensure!(
                    names.len() < MAX_LIST_ENTRIES,
                    "flow library exceeds listing limit"
                );
                names.push(name);
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub(crate) fn browse(&self, query: &str, offset: usize, limit: usize) -> Result<BrowsePage> {
        ensure!(query.len() <= 128, "browse query exceeds 128 UTF-8 bytes");
        ensure!(
            !query.chars().any(char::is_control),
            "browse query contains control characters"
        );
        ensure!(offset <= 4096, "browse offset exceeds limit");
        ensure!(
            (1..=MAX_BROWSE_ENTRIES).contains(&limit),
            "browse limit must be 1 to 12"
        );

        let query_folded = query.to_lowercase();
        let matches: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|name| name.to_lowercase().contains(&query_folded))
            .collect();
        let total = matches.len();
        let entries = matches
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(
                |name| match self.load(&name).and_then(|flow| preview(&flow.mapper)) {
                    Ok(preview) => BrowseEntry {
                        name,
                        preview: Some(preview),
                        error: None,
                    },
                    Err(error) => BrowseEntry {
                        name,
                        preview: None,
                        error: Some(bounded_error(&error.to_string())),
                    },
                },
            )
            .collect();
        Ok(BrowsePage {
            query: query.to_owned(),
            offset,
            limit,
            total,
            entries,
        })
    }

    pub(crate) fn save(&self, flow: &Flow) -> Result<()> {
        let bytes = encode(flow)?;
        fs::create_dir_all(&self.directory)?;
        self.require_library_directory()?;
        crate::gui_macro_file::atomic_save(&self.path(&flow.name)?, &bytes)
    }

    pub(crate) fn load(&self, name: &str) -> Result<Flow> {
        let path = self.path(name)?;
        self.require_library_directory()?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file() && is_plain(&metadata),
            "saved flow must be a regular file"
        );
        let flow = Self::import(&path)?;
        ensure!(flow.name == name, "flow name does not match requested name");
        Ok(flow)
    }

    pub(crate) fn export(path: &Path, flow: &Flow) -> Result<()> {
        validate_path(path)?;
        let bytes = encode(flow)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        crate::gui_macro_file::atomic_save(path, &bytes)
    }

    pub(crate) fn import(path: &Path) -> Result<Flow> {
        validate_path(path)?;
        ensure!(
            fs::metadata(path)?.is_file(),
            "flow transfer source must be a regular file"
        );
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take((MAX_FLOW_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_FLOW_BYTES,
            "flow file exceeds size limit"
        );
        let flow: Flow = serde_json::from_slice(&bytes).context("invalid flow file")?;
        validate_flow(&flow)?;
        Ok(flow)
    }

    fn require_library_directory(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.directory)?;
        ensure!(
            metadata.is_dir() && is_plain(&metadata),
            "flow library must be a directory"
        );
        Ok(())
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        let mut encoded = String::with_capacity(name.len() * 2);
        for byte in name.as_bytes() {
            use std::fmt::Write as _;
            write!(encoded, "{byte:02x}")?;
        }
        Ok(self.directory.join(format!("{encoded}.json")))
    }
}

fn bounded_error(error: &str) -> String {
    error.chars().take(256).collect()
}

pub(crate) fn preview(mapper: &Definition) -> Result<Preview> {
    mapper
        .validate()
        .context("invalid mapper definition for preview")?;
    use std::collections::{BTreeMap, BTreeSet};

    let node_count = mapper.nodes.len();
    let edge_count = mapper.edges.len();
    let group_count = mapper.groups.len();
    let mut collapsed_members = BTreeMap::<&str, &str>::new();
    for group in mapper.groups.iter().filter(|group| group.collapsed) {
        for member in &group.nodes {
            collapsed_members.insert(member, group.id.as_str());
        }
    }

    let group_anchors: BTreeMap<_, _> = mapper
        .groups
        .iter()
        .filter(|group| group.collapsed)
        .filter_map(|group| {
            group
                .nodes
                .first()
                .map(|id| (id.as_str(), group.id.as_str()))
        })
        .collect();
    let mut projected = Vec::new();
    let mut index = BTreeMap::<String, usize>::new();
    let mut group_positions = BTreeMap::<&str, [f64; 2]>::new();
    for (i, node) in mapper.nodes.iter().enumerate() {
        let normalized = node
            .position
            .filter(|p| p.iter().all(|v| v.is_finite()))
            .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
            .unwrap_or_else(|| fallback_position(i, node_count));
        if let Some(group_id) = collapsed_members.get(node.id.as_str()) {
            if group_anchors.get(node.id.as_str()) == Some(group_id) {
                group_positions.insert(group_id, normalized);
            }
            continue;
        }
        let kind = serde_json::to_value(&node.kind)?
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        index.insert(node.id.clone(), projected.len());
        projected.push(PreviewNode {
            id: node.id.clone(),
            kind,
            position: normalized,
        });
    }
    for group in mapper.groups.iter().filter(|group| group.collapsed) {
        let id = format!("group:{}", group.id);
        let pos = group_positions
            .get(group.id.as_str())
            .copied()
            .unwrap_or_else(|| fallback_position(projected.len(), node_count));
        index.insert(id.clone(), projected.len());
        projected.push(PreviewNode {
            id,
            kind: "group".into(),
            position: pos,
        });
    }

    let mut edges = BTreeSet::new();
    for edge in &mapper.edges {
        let from = collapsed_members
            .get(edge.from.node.as_str())
            .map(|id| format!("group:{id}"))
            .unwrap_or_else(|| edge.from.node.clone());
        let to = collapsed_members
            .get(edge.to.node.as_str())
            .map(|id| format!("group:{id}"))
            .unwrap_or_else(|| edge.to.node.clone());
        if from == to {
            continue;
        }
        if let (Some(&from), Some(&to)) = (index.get(&from), index.get(&to)) {
            edges.insert([from, to]);
        }
    }
    Ok(Preview {
        nodes: projected,
        edges: edges.into_iter().collect(),
        node_count,
        edge_count,
        group_count,
    })
}

fn fallback_position(index: usize, count: usize) -> [f64; 2] {
    let columns = (count.max(1) as f64).sqrt().ceil() as usize;
    let rows = count.max(1).div_ceil(columns);
    [
        ((index % columns) as f64 + 0.5) / columns as f64,
        ((index / columns) as f64 + 0.5) / rows as f64,
    ]
}

fn encode(flow: &Flow) -> Result<Vec<u8>> {
    validate_flow(flow)?;
    let bytes = serde_json::to_vec(flow)?;
    ensure!(
        bytes.len() <= MAX_FLOW_BYTES,
        "flow file exceeds size limit"
    );
    Ok(bytes)
}

fn is_plain(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

fn validate_flow(flow: &Flow) -> Result<()> {
    ensure!(flow.version == FLOW_VERSION, "unsupported flow version");
    validate_name(&flow.name)?;
    flow.mapper
        .validate()
        .context("invalid mapper definition")?;
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= MAX_NAME_BYTES,
        "flow name must be 1 to 64 UTF-8 bytes"
    );
    ensure!(
        name.trim() == name,
        "flow name cannot have surrounding whitespace"
    );
    ensure!(
        !name.chars().any(char::is_control),
        "flow name cannot contain control characters"
    );
    Ok(())
}

fn decode_name(encoded: &str) -> Option<String> {
    if encoded.is_empty()
        || encoded.len() > MAX_NAME_BYTES * 2
        || !encoded.len().is_multiple_of(2)
        || !encoded
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

fn validate_path(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    ensure!(path.file_name().is_some(), "choose a flow file path");
    ensure!(
        !path.as_os_str().encode_wide().any(|unit| unit == 0),
        "flow path contains NUL"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::mapper::{Definition, Disposition, Node, NodeGroup, NodeKind};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "protoinputmultiplex-flow-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn round_trip_sorted_listing_and_export_import() {
        let root = root();
        let store = Store::new(&root);
        assert!(store.list().unwrap().is_empty());
        let flow = Flow::new("Mouse flow 🐭".into(), Definition::default());
        store.save(&flow).unwrap();
        assert_eq!(store.load(&flow.name).unwrap(), flow);
        store
            .save(&Flow::new("A second flow".into(), Definition::default()))
            .unwrap();
        fs::write(store.directory.join("x猫.json"), b"not a flow").unwrap();
        assert_eq!(
            store.list().unwrap(),
            vec!["A second flow".to_owned(), flow.name.clone()]
        );
        let export = root.join("portable.json");
        Store::export(&export, &flow).unwrap();
        assert_eq!(Store::import(&export).unwrap(), flow);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_names_versions_schema_and_oversized_files() {
        let root = root();
        let store = Store::new(&root);
        for name in [String::new(), " ../x".into(), "a\n".into(), "x".repeat(65)] {
            assert!(store.save(&Flow::new(name, Definition::default())).is_err());
        }
        let path_like = Flow::new("a/b\\c".into(), Definition::default());
        store.save(&path_like).unwrap();
        assert_eq!(store.load(&path_like.name).unwrap(), path_like);
        let mut unsupported = Flow::new("okay".into(), Definition::default());
        unsupported.version = 8;
        assert!(store.save(&unsupported).is_err());
        fs::create_dir_all(&store.directory).unwrap();
        fs::write(store.path("okay").unwrap(), br#"{"version":1,"name":"okay","mapper":{"version":1,"nodes":[],"edges":[]},"extra":true}"#).unwrap();
        assert!(store.load("okay").is_err());
        let file = fs::File::create(store.path("large").unwrap()).unwrap();
        file.set_len((MAX_FLOW_BYTES + 1) as u64).unwrap();
        assert!(store.load("large").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_write_preserves_previous_flow() {
        let root = root();
        let store = Store::new(&root);
        let original = Flow::new("safe".into(), Definition::default());
        store.save(&original).unwrap();
        let path = store.path("safe").unwrap();
        let before = fs::read(&path).unwrap();
        // An invalid replacement is rejected before the atomic commit can touch the target.
        let invalid = Flow {
            version: 2,
            ..original.clone()
        };
        assert!(store.save(&invalid).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browse_searches_pages_and_keeps_corrupt_entries_local() {
        let root = root();
        let store = Store::new(&root);
        store
            .save(&Flow::new("Alpha".into(), Definition::default()))
            .unwrap();
        store
            .save(&Flow::new("Beta".into(), Definition::default()))
            .unwrap();
        store
            .save(&Flow::new("Gamma".into(), Definition::default()))
            .unwrap();
        fs::write(store.path("Beta").unwrap(), b"broken").unwrap();
        let page = store.browse("a", 1, 2).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["Beta", "Gamma"]
        );
        assert!(page.entries[0].preview.is_none());
        assert!(page.entries[0].error.as_ref().unwrap().len() <= 256);
        assert!(page.entries[1].preview.is_some());
        assert_eq!(store.browse("", 0, 1).unwrap().entries[0].name, "Alpha");
        assert!(store.browse("\n", 0, 1).is_err());
        assert!(store.browse("", 0, 13).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collapsed_group_projects_as_one_synthetic_node() {
        let mut mapper = Definition {
            nodes: vec![Node {
                id: "pointer".into(),
                kind: NodeKind::Pointer {
                    user: "private-user".into(),
                    disposition: Disposition::Passthrough,
                },
                position: Some([1.5, -0.5]),
            }],
            groups: vec![NodeGroup {
                id: "g".into(),
                label: "Friendly group label".into(),
                nodes: vec!["pointer".into()],
                collapsed: true,
            }],
            ..Definition::default()
        };
        let result = preview(&mapper).unwrap();
        assert_eq!(result.node_count, 1);
        assert_eq!(result.group_count, 1);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id, "group:g");
        assert_eq!(result.nodes[0].kind, "group");
        assert_eq!(result.nodes[0].position, [1.0, 0.0]);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private-user")
        );

        mapper.nodes[0].position = None;
        let result = preview(&mapper).unwrap();
        assert_eq!(result.nodes[0].position, [0.5, 0.5]);

        mapper.nodes.push(Node {
            id: "second".into(),
            kind: NodeKind::Pointer {
                user: "another-private-user".into(),
                disposition: Disposition::Passthrough,
            },
            position: Some([0.25, 0.75]),
        });
        mapper.groups[0].nodes.insert(0, "second".into());
        let result = preview(&mapper).unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].position, [0.25, 0.75]);
    }
}
