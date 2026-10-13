use std::path::PathBuf;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// Compact stable index into an [`Arena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct NodeIndex(u32);

impl NodeIndex {
    pub fn from_usize(value: usize) -> Self {
        Self(u32::try_from(value).expect("arena exceeds u32 node capacity"))
    }

    /// Convert to an index suitable for a slice.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Filesystem entry kind. Symlinks are entries and are never followed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    File,
    Directory,
    Symlink,
    #[default]
    Other,
}

impl NodeKind {
    /// Whether the entry contributes file allocation to accounting.
    #[must_use]
    pub const fn is_file_like(self) -> bool {
        matches!(self, Self::File | Self::Symlink | Self::Other)
    }
}

bitflags! {
    /// Safety and filesystem-state flags attached to a node.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct NodeFlags: u16 {
        const DATALESS = 1 << 0;
        const FIRMLINK = 1 << 1;
        const MOUNT_BOUNDARY = 1 << 2;
        const EXCLUDED = 1 << 3;
        const METADATA_FALLBACK = 1 << 4;
    }
}

/// Dense node stored in a flat arena rather than an allocation-heavy tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Node {
    #[serde(skip)]
    pub(crate) parent: Option<NodeIndex>,
    #[serde(skip)]
    pub(crate) first_child: Option<NodeIndex>,
    #[serde(skip)]
    pub(crate) next_sibling: Option<NodeIndex>,
    #[serde(skip)]
    pub(crate) name_offset: u32,
    #[serde(skip)]
    pub(crate) name_len: u32,
    pub kind: NodeKind,
    pub apparent_size: u64,
    pub allocated_size: u64,
    pub private_size: Option<u64>,
    pub device: u64,
    pub inode: u64,
    pub link_count: u64,
    pub clone_id: Option<u64>,
    pub clone_refcount: Option<u32>,
    pub modified_unix_seconds: i64,
    pub flags: NodeFlags,
}

/// Flat tree with names interned in one byte buffer.
#[derive(Debug, Serialize)]
pub struct Arena {
    pub nodes: Vec<Node>,
    #[serde(skip)]
    names: Vec<u8>,
    #[serde(skip)]
    root_path: PathBuf,
}

impl Arena {
    /// Create an arena with a directory root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root_path = root.into();
        let root_name = root_path.to_string_lossy().into_owned();
        let mut arena = Self {
            nodes: Vec::new(),
            names: Vec::new(),
            root_path,
        };
        arena.push_raw(
            None,
            &root_name,
            Node {
                kind: NodeKind::Directory,
                ..Node::default()
            },
        );
        arena
    }

    /// Root node index.
    #[must_use]
    pub const fn root(&self) -> NodeIndex {
        NodeIndex(0)
    }

    /// Add a child and link it into its parent's sibling list.
    pub fn push_child(&mut self, parent: NodeIndex, name: &str, node: Node) -> NodeIndex {
        self.push_raw(Some(parent), name, node)
    }

    fn push_raw(&mut self, parent: Option<NodeIndex>, name: &str, mut node: Node) -> NodeIndex {
        let index = NodeIndex::from_usize(self.nodes.len());
        node.parent = parent;
        node.name_offset = u32::try_from(self.names.len()).expect("name pool exceeds u32");
        node.name_len = u32::try_from(name.len()).expect("single filename exceeds u32");
        self.names.extend_from_slice(name.as_bytes());

        if let Some(parent) = parent {
            node.next_sibling = self.nodes[parent.as_usize()].first_child;
            self.nodes[parent.as_usize()].first_child = Some(index);
        }
        self.nodes.push(node);
        index
    }

    /// Replace measured attributes while preserving names and destination links.
    pub fn update_metadata(&mut self, index: NodeIndex, mut node: Node) {
        let old = &self.nodes[index.as_usize()];
        node.parent = old.parent;
        node.first_child = old.first_child;
        node.next_sibling = old.next_sibling;
        node.name_offset = old.name_offset;
        node.name_len = old.name_len;
        self.nodes[index.as_usize()] = node;
    }

    /// Copy a subtree into another arena without retaining its old indices.
    /// Iterative traversal also handles very deep fixture trees safely.
    pub fn copy_children_into(&self, root: NodeIndex, destination: &mut Arena, parent: NodeIndex) {
        let mut pending = vec![(root, parent)];
        while let Some((source, parent)) = pending.pop() {
            for child in self.children(source) {
                let mut node = self.nodes[child.as_usize()].clone();
                node.first_child = None;
                node.next_sibling = None;
                let index = destination.push_child(parent, self.name(child), node);
                pending.push((child, index));
            }
        }
    }

    /// Borrow a node's interned name.
    #[must_use]
    pub fn name(&self, index: NodeIndex) -> &str {
        let node = &self.nodes[index.as_usize()];
        let start = node.name_offset as usize;
        let end = start + node.name_len as usize;
        std::str::from_utf8(&self.names[start..end]).unwrap_or("<non-utf8>")
    }

    /// Iterate direct children.
    pub fn children(&self, parent: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        let mut next = self.nodes[parent.as_usize()].first_child;
        std::iter::from_fn(move || {
            let current = next?;
            next = self.nodes[current.as_usize()].next_sibling;
            Some(current)
        })
    }

    /// Return the parent index, or `None` for the root.
    #[must_use]
    pub fn parent(&self, index: NodeIndex) -> Option<NodeIndex> {
        self.nodes[index.as_usize()].parent
    }

    /// Reconstruct the absolute or scan-root-relative path for a node.
    #[must_use]
    pub fn path(&self, index: NodeIndex) -> PathBuf {
        if index == self.root() {
            return self.root_path.clone();
        }
        let mut parts = Vec::new();
        let mut cursor = Some(index);
        while let Some(current) = cursor {
            if current == self.root() {
                break;
            }
            parts.push(self.name(current).to_owned());
            cursor = self.nodes[current.as_usize()].parent;
        }
        let mut path = self.root_path.clone();
        for part in parts.iter().rev() {
            path.push(part);
        }
        path
    }
}
