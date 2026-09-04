use serde::{Deserialize, Serialize};

/// A directory structure represented as a vector of nodes, where each node can reference its parent.
/// The position of a node in the vector serves as its ID.
/// Nodes without parents represent top-level entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Directory {
    pub nodes: Vec<DirectoryNode>,
}

/// A node in the directory structure, representing either a file or a directory.
/// The node's position in the parent Directory's nodes vector serves as its ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryNode {
    pub name: String,
    pub kind: NodeKind,
    pub parent: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Directory,
}

impl Directory {
    /// Recursively builds the directory structure from a filesystem path.
    /// Returns an error if the directory does not exist or is not a directory.
    pub fn from_dir_path(root: &std::path::Path) -> std::io::Result<Self> {
        if !root.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Directory does not exist: {}", root.display()),
            ));
        }
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Path is not a directory: {}", root.display()),
            ));
        }
        let is_empty = std::fs::read_dir(root).map(|mut it| it.next().is_none())?;
        if is_empty {
            return Ok(Self { nodes: vec![] });
        }
        let mut nodes = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                Self::build_map(&entry.path(), &mut nodes, None);
            }
        }
        Ok(Self { nodes })
    }

    /// Recursively builds the directory structure from a filesystem path.
    /// Returns the index of the created node in the nodes vector.
    fn build_map(
        path: &std::path::Path,
        nodes: &mut Vec<DirectoryNode>,
        parent: Option<usize>,
    ) -> usize {
        let name = path
            .file_name()
            .expect("path should have a file name")
            .to_string_lossy()
            .into_owned();

        let node_id = nodes.len();
        let is_dir = path.is_dir();

        if is_dir {
            nodes.push(DirectoryNode {
                name,
                kind: NodeKind::Directory,
                parent,
            });

            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    Self::build_map(&entry.path(), nodes, Some(node_id));
                }
            }
        } else {
            nodes.push(DirectoryNode {
                name,
                kind: NodeKind::File,
                parent,
            });
        }

        node_id
    }

    pub fn nodes(&self) -> impl Iterator<Item = &DirectoryNode> {
        self.nodes.iter()
    }

    pub fn children(&self, node_id: usize) -> impl Iterator<Item = &DirectoryNode> {
        self.nodes
            .iter()
            .filter(move |node| node.parent == Some(node_id))
    }

    #[must_use]
    pub fn parent(&self, node_id: usize) -> Option<&DirectoryNode> {
        self.nodes
            .get(node_id)
            .and_then(|node| node.parent.map(|p| &self.nodes[p]))
    }

    /// Returns the top-level nodes (nodes without parents).
    pub fn top_level(&self) -> impl Iterator<Item = &DirectoryNode> {
        self.nodes.iter().filter(|node| node.parent.is_none())
    }

    /// Merges another directory into this one.
    /// Nodes from the other directory are appended to this one's nodes vector.
    /// Parent references in the other directory's nodes are adjusted by adding the length
    /// of this directory's nodes vector.
    pub fn merge(&mut self, other: Directory) {
        let offset = self.nodes.len();
        self.nodes.extend(other.nodes.into_iter().map(|mut node| {
            if let Some(parent) = node.parent {
                node.parent = Some(parent + offset);
            }
            node
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge() {
        // Create first directory with two nodes:
        // - A top-level file "file1"
        // - A directory "dir1" containing "file2"
        let mut dir1 = Directory {
            nodes: vec![
                DirectoryNode {
                    name: "file1".to_string(),
                    kind: NodeKind::File,
                    parent: None,
                },
                DirectoryNode {
                    name: "dir1".to_string(),
                    kind: NodeKind::Directory,
                    parent: None,
                },
                DirectoryNode {
                    name: "file2".to_string(),
                    kind: NodeKind::File,
                    parent: Some(1),
                },
            ],
        };

        // Create second directory with two nodes:
        // - A top-level file "file3"
        // - A directory "dir2" containing "file4"
        let dir2 = Directory {
            nodes: vec![
                DirectoryNode {
                    name: "file3".to_string(),
                    kind: NodeKind::File,
                    parent: None,
                },
                DirectoryNode {
                    name: "dir2".to_string(),
                    kind: NodeKind::Directory,
                    parent: None,
                },
                DirectoryNode {
                    name: "file4".to_string(),
                    kind: NodeKind::File,
                    parent: Some(1),
                },
            ],
        };

        // Merge dir2 into dir1
        dir1.merge(dir2);

        // Verify the merged structure:
        // 1. All nodes are present
        assert_eq!(dir1.nodes.len(), 6);

        // 2. Top-level nodes (no parents) are correct
        let top_level: Vec<_> = dir1.top_level().collect();
        assert_eq!(top_level.len(), 4);
        assert!(top_level.iter().any(|n| n.name == "file1"));
        assert!(top_level.iter().any(|n| n.name == "dir1"));
        assert!(top_level.iter().any(|n| n.name == "file3"));
        assert!(top_level.iter().any(|n| n.name == "dir2"));

        // 3. Parent relationships are preserved
        // file2 should still point to dir1 (index 1)
        assert_eq!(dir1.nodes[2].parent, Some(1));
        // file4 should point to dir2 (index 4, which is 1 + offset of 3 - length of dir1)
        assert_eq!(dir1.nodes[5].parent, Some(4));
    }
}
