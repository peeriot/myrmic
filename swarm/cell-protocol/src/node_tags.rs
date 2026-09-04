//! Tags a node carries beyond the ones its configuration file gives it.
//!
//! A node's configuration is fixed at boot and invisible from the network, so
//! retagging a running fleet cannot work by editing it. Instead each node may
//! have an overlay row — tags to add, tags to drop — in the `sys` namespace,
//! which every db node replicates, so a change reaches the node itself and
//! everything that reads tags about it.
//!
//! An overlay lasts one incarnation of the node it names: a node deletes its
//! own row at boot, so a restart returns it to its configured tags. A retag is
//! therefore a change to a running fleet and never a durable setting — the
//! configuration file is where a tag that must outlive a restart belongs.
//!
//! The overlay lives apart from the exec registry row on purpose: that row is
//! exclusive to nodes running an exec, while a db-only node has tags that
//! steer replication.
//!
//! [`effective`] is the one merge every runtime — linux exec, db plugin and
//! embedded firmware alike — resolves tags through.

use db_commons::models::Scope;
use serde::{Deserialize, Serialize};

use crate::RuntimeId;
use crate::sys::string::{String, ToString};
use crate::sys::vec::Vec;

/// Database of the node tag overlays
const NODE_TAGS_DB: &str = "nodes";
/// Table of the per-node tag overlays, keyed by runtime id
pub const NODE_TAGS_TABLE: &str = "tags";

/// Returns the DB scope holding the node tag overlays.
///
/// Lives in the `sys` namespace, which every db node replicates
/// unconditionally, so an overlay reaches nodes that join later.
pub fn node_tags_scope() -> Scope {
    Scope::new(db_commons::NAMESPACE_SYS, NODE_TAGS_DB, "p")
}

/// One node's dynamically configured tags, as stored in [`node_tags_scope`]'s
/// [`NODE_TAGS_TABLE`].
///
/// Additions and removals are kept apart rather than collapsed into a final
/// tag list so that the node's configuration keeps its meaning: editing the
/// config file still takes effect on restart, and a tag can be dropped whether
/// it came from configuration or from an earlier addition. It also lets the
/// overlay be written by something that cannot see the node's configuration at
/// all, which is every writer, since configuration never leaves the node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTagOverlay {
    /// The node these tags belong to.
    pub node: RuntimeId,
    /// Tags to carry on top of the configured ones.
    pub added: Vec<String>,
    /// Tags to drop, whatever their origin.
    pub removed: Vec<String>,
}

impl NodeTagOverlay {
    /// An empty overlay for `node`.
    #[must_use]
    pub fn new(node: RuntimeId) -> Self {
        Self {
            node,
            added: Vec::new(),
            removed: Vec::new(),
        }
    }

    /// The overlay's key in the node tags table.
    #[must_use]
    pub fn key(&self) -> String {
        self.node.to_string()
    }

    /// Whether this overlay would change nothing. Such an overlay is deleted
    /// rather than stored — a row saying "no change" is only replication noise.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    /// Records that the node should carry `tag`.
    pub fn add(&mut self, tag: &str) {
        self.removed.retain(|existing| existing != tag);
        if !self.added.iter().any(|existing| existing == tag) {
            self.added.push(String::from(tag));
        }
    }

    /// Records that the node should not carry `tag`, whatever its origin.
    pub fn remove(&mut self, tag: &str) {
        self.added.retain(|existing| existing != tag);
        if !self.removed.iter().any(|existing| existing == tag) {
            self.removed.push(String::from(tag));
        }
    }
}

/// The tags a node ends up carrying: its `configured` tags with `overlay`
/// applied, then `intrinsic` forced back in.
///
/// Intrinsics are facts about the node rather than preferences — its platform,
/// its compiled-in capabilities, its own runtime tag — so an overlay may not
/// drop one. A node that lost its platform tag would simply stop being a
/// candidate for any placement, which is never what removing a tag meant.
pub fn effective(
    overlay: Option<&NodeTagOverlay>,
    configured: &[String],
    intrinsic: &[String],
) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();

    let added = overlay.map_or(&[][..], |overlay| &overlay.added);
    let removed = overlay.map_or(&[][..], |overlay| &overlay.removed);

    for tag in configured.iter().chain(added) {
        if !removed.contains(tag) && !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }

    for tag in intrinsic {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }

    tags
}

#[cfg(not(target_os = "none"))]
pub use live::LiveTags;

#[cfg(not(target_os = "none"))]
mod live {
    use std::sync::Arc;

    use tokio::sync::watch;

    /// This node's tags as they stand right now, shared by every part of the
    /// runtime that acts on them.
    ///
    /// A node has one tag set, not one per plugin: the tags that decide which
    /// cells it may run are the same tags that decide which data it replicates.
    /// Exactly one task writes it — the watcher following the overlay row —
    /// while readers take the current value or await the next change.
    #[derive(Debug, Clone)]
    pub struct LiveTags(Arc<watch::Sender<Arc<[String]>>>);

    impl LiveTags {
        /// A live set starting at `tags`.
        #[must_use]
        pub fn new(tags: Vec<String>) -> Self {
            Self(Arc::new(watch::Sender::new(tags.into())))
        }

        /// The tags as they stand.
        #[must_use]
        pub fn get(&self) -> Arc<[String]> {
            self.0.borrow().clone()
        }

        /// Replaces the set, reporting whether it actually changed. An
        /// unchanged set wakes nobody.
        pub fn set(&self, tags: Vec<String>) -> bool {
            self.0.send_if_modified(|current| {
                if current.as_ref() == tags.as_slice() {
                    return false;
                }
                *current = tags.into();
                true
            })
        }

        /// A handle that resolves whenever the set changes.
        #[must_use]
        pub fn subscribe(&self) -> watch::Receiver<Arc<[String]>> {
            self.0.subscribe()
        }
    }

    impl Default for LiveTags {
        fn default() -> Self {
            Self::new(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| String::from(*tag)).collect()
    }

    fn overlay(added: &[&str], removed: &[&str]) -> NodeTagOverlay {
        NodeTagOverlay {
            node: zenoh_protocol::core::ZenohIdProto::try_from(&[7u8; 8][..])
                .unwrap()
                .into(),
            added: tags(added),
            removed: tags(removed),
        }
    }

    #[test]
    fn no_overlay_leaves_the_configured_tags_alone() {
        let effective = effective(None, &tags(&["region-1", "gpu"]), &tags(&["linux"]));
        assert_eq!(effective, tags(&["region-1", "gpu", "linux"]));
    }

    #[test]
    fn additions_follow_the_configured_tags() {
        let overlay = overlay(&["hello", "region-1"], &[]);
        let effective = effective(Some(&overlay), &tags(&["gpu"]), &[]);
        assert_eq!(effective, tags(&["gpu", "hello", "region-1"]));
    }

    #[test]
    fn removal_drops_a_configured_tag() {
        let overlay = overlay(&[], &["region-2"]);
        let effective = effective(Some(&overlay), &tags(&["region-2", "gpu"]), &[]);
        assert_eq!(effective, tags(&["gpu"]));
    }

    #[test]
    fn intrinsic_tags_survive_removal() {
        // Dropping the platform tag would make the node unplaceable, which is
        // never what removing a tag was asking for.
        let overlay = overlay(&[], &["linux"]);
        let effective = effective(Some(&overlay), &tags(&["linux", "gpu"]), &tags(&["linux"]));
        assert_eq!(effective, tags(&["gpu", "linux"]));
    }

    #[test]
    fn a_tag_carried_twice_appears_once() {
        let overlay = overlay(&["gpu"], &[]);
        let effective = effective(Some(&overlay), &tags(&["gpu"]), &tags(&["gpu"]));
        assert_eq!(effective, tags(&["gpu"]));
    }

    #[test]
    fn adding_a_removed_tag_takes_it_off_the_removals() {
        let mut overlay = overlay(&[], &["region-1"]);
        overlay.add("region-1");
        assert_eq!(overlay.added, tags(&["region-1"]));
        assert!(overlay.removed.is_empty());
    }

    #[test]
    fn removing_an_added_tag_takes_it_off_the_additions() {
        let mut overlay = overlay(&["region-1"], &[]);
        overlay.remove("region-1");
        assert_eq!(overlay.removed, tags(&["region-1"]));
        assert!(overlay.added.is_empty());
    }

    #[test]
    fn repeating_an_edit_changes_nothing() {
        let mut overlay = overlay(&[], &[]);
        overlay.add("gpu");
        overlay.add("gpu");
        assert_eq!(overlay.added, tags(&["gpu"]));
    }

    #[test]
    fn an_overlay_that_changes_nothing_is_empty() {
        let mut overlay = overlay(&[], &[]);
        assert!(overlay.is_empty());
        overlay.add("gpu");
        assert!(!overlay.is_empty());
    }

    #[test]
    fn live_tags_report_whether_they_changed() {
        let live = LiveTags::new(tags(&["gpu"]));
        assert!(!live.set(tags(&["gpu"])));
        assert!(live.set(tags(&["gpu", "region-1"])));
        assert_eq!(live.get().as_ref(), tags(&["gpu", "region-1"]).as_slice());
    }
}
