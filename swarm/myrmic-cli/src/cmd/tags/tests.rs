use super::*;

use std::str::FromStr as _;

use cell_protocol::{CapabilityTag, ExecutionCapabilities};

/// Runtime ids are hex without a leading zero; one repeated byte gives a
/// stable, distinct id per test node, long enough to take a prefix of.
fn id(byte: u8) -> RuntimeId {
    let hex = format!("{:02x}", 0xa0 | (byte & 0x0f));
    RuntimeId::from_str(&hex.repeat(4)).expect("a valid runtime id")
}

fn tags(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|tag| String::from(*tag)).collect()
}

/// A node reporting `carried` under `name`; no name means no exec registry
/// entry, as a db-only node has.
fn node(byte: u8, name: Option<&str>, carried: &[&str]) -> Node {
    let exec = name.map(|name| {
        let capabilities = carried.iter().map(|tag| CapabilityTag::new(*tag)).collect();

        ExecRuntimeInfo::new(
            id(byte),
            Some(String::from(name)),
            ExecutionCapabilities::new(capabilities),
        )
    });

    Node { id: id(byte), exec }
}

fn overlays(rows: &[(RuntimeId, &[&str], &[&str])]) -> HashMap<RuntimeId, NodeTagOverlay> {
    rows.iter()
        .map(|(node, added, removed)| {
            let overlay = NodeTagOverlay {
                node: *node,
                added: tags(added),
                removed: tags(removed),
            };
            (*node, overlay)
        })
        .collect()
}

/// The overlay a change list leaves for `node`, so assertions can talk about
/// the stored entry rather than the change list.
fn applied(changes: &[Change], node: RuntimeId) -> Option<&NodeTagOverlay> {
    changes.iter().find_map(|change| match change {
        Change::Set(overlay) if overlay.node == node => Some(overlay),
        _ => None,
    })
}

#[test]
fn a_tag_is_added_to_every_named_node() {
    let overlays = overlays(&[]);
    let changes = edits(&[id(1), id(2)], &tags(&["hello"]), &[], &overlays).unwrap();

    assert_eq!(changes.len(), 2);
    assert_eq!(applied(&changes, id(1)).unwrap().added, tags(&["hello"]));
    assert_eq!(applied(&changes, id(2)).unwrap().added, tags(&["hello"]));
}

#[test]
fn adding_keeps_the_tags_a_node_already_had() {
    let overlays = overlays(&[(id(1), &["region-1"], &[])]);
    let changes = edits(&[id(1)], &tags(&["hello"]), &[], &overlays).unwrap();

    assert_eq!(
        applied(&changes, id(1)).unwrap().added,
        tags(&["region-1", "hello"])
    );
}

#[test]
fn excluding_records_a_removal() {
    let overlays = overlays(&[]);
    let changes = edits(&[id(3)], &[], &tags(&["region-2"]), &overlays).unwrap();

    assert_eq!(
        applied(&changes, id(3)).unwrap().removed,
        tags(&["region-2"])
    );
}

#[test]
fn excluding_an_added_tag_records_a_removal_rather_than_undoing_it() {
    // The tag may also come from the node's configuration, which is invisible
    // from here, so "must not carry" is the only honest reading.
    let overlays = overlays(&[(id(1), &["hello"], &[])]);
    let changes = edits(&[id(1)], &[], &tags(&["hello"]), &overlays).unwrap();

    let overlay = applied(&changes, id(1)).unwrap();
    assert!(overlay.added.is_empty());
    assert_eq!(overlay.removed, tags(&["hello"]));
}

#[test]
fn resetting_drops_a_tagged_node_back_to_its_configuration() {
    let overlays = overlays(&[(id(1), &["hello"], &["region-2"])]);

    assert_eq!(reset(&[id(1)], &overlays), vec![Change::Drop(id(1))]);
}

#[test]
fn resetting_a_node_that_was_never_tagged_writes_nothing() {
    let overlays = overlays(&[]);

    assert!(reset(&[id(1)], &overlays).is_empty());
}

#[test]
fn undoing_an_edit_on_an_untagged_node_writes_nothing() {
    let overlays = overlays(&[]);
    let changes = edits(&[id(1)], &[], &[], &overlays).unwrap();

    assert!(changes.is_empty());
}

#[test]
fn a_system_tag_is_refused() {
    // `@` names a node, never a tag a node carries.
    let overlays = overlays(&[]);
    let err = edits(&[id(1)], &tags(&["@abc"]), &[], &overlays).unwrap_err();

    assert!(err.to_string().contains("@abc"), "{err}");
}

#[test]
fn a_node_resolves_by_name_with_or_without_the_prefix() {
    let nodes = vec![node(1, Some("node-1"), &[]), node(2, Some("node-2"), &[])];

    assert_eq!(resolve_one("@node-1", &nodes).unwrap(), id(1));
    assert_eq!(resolve_one("node-2", &nodes).unwrap(), id(2));
}

#[test]
fn a_node_resolves_by_id_prefix() {
    let nodes = vec![node(1, Some("node-1"), &[]), node(2, Some("node-2"), &[])];
    let prefix = &id(2).to_string()[..4];

    assert_eq!(resolve_one(prefix, &nodes).unwrap(), id(2));
}

#[test]
fn an_unknown_node_is_an_error() {
    let nodes = vec![node(1, Some("node-1"), &[])];
    let err = resolve_one("@nope", &nodes).unwrap_err();

    assert!(err.to_string().contains("no node called '@nope'"), "{err}");
}

#[test]
fn a_node_with_no_registry_entry_still_resolves() {
    // A db-only node has no exec to register a name, but it holds replicas,
    // so it must still be taggable — by id.
    let nodes = vec![node(7, None, &[])];

    assert_eq!(resolve_one(&id(7).to_string(), &nodes).unwrap(), id(7));
}

#[test]
fn carried_tags_render_plainly() {
    let node = node(1, Some("node-1"), &["linux", "region-1"]);

    assert_eq!(tag_cells(&node, None), tags(&["linux", "region-1"]));
}

#[test]
fn a_tag_the_node_has_not_taken_up_is_pending() {
    let node = node(1, Some("node-1"), &["linux"]);
    let overlay = NodeTagOverlay {
        node: id(1),
        added: tags(&["hello"]),
        removed: Vec::new(),
    };

    assert_eq!(
        tag_cells(&node, Some(&overlay)),
        tags(&["linux", "hello (pending)"])
    );
}

#[test]
fn a_tag_the_node_still_reports_is_removing() {
    let node = node(1, Some("node-1"), &["linux", "region-2"]);
    let overlay = NodeTagOverlay {
        node: id(1),
        added: Vec::new(),
        removed: tags(&["region-2"]),
    };

    assert_eq!(
        tag_cells(&node, Some(&overlay)),
        tags(&["linux", "region-2 (removing)"])
    );
}

#[test]
fn a_tag_the_node_has_taken_up_stops_being_pending() {
    let node = node(1, Some("node-1"), &["linux", "hello"]);
    let overlay = NodeTagOverlay {
        node: id(1),
        added: tags(&["hello"]),
        removed: Vec::new(),
    };

    assert_eq!(tag_cells(&node, Some(&overlay)), tags(&["linux", "hello"]));
}

#[test]
fn an_untagged_db_only_node_shows_nothing() {
    let node = node(7, None, &[]);

    assert!(tag_cells(&node, None).is_empty());
}
