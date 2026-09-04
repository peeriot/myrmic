use super::*;

fn tags(tags: &[&str]) -> Vec<String> {
    tags.iter().map(|tag| String::from(*tag)).collect()
}

fn configured(rows: &[(&str, &[&str])]) -> BTreeMap<String, ReplicaEntry> {
    rows.iter()
        .map(|(identifier, row_tags)| {
            let entry = ReplicaEntry::new(parse(identifier).unwrap(), tags(row_tags), identifier);
            (entry.key(), entry)
        })
        .collect()
}

/// Applies changes to a snapshot the way `handle` does, so the assertions can
/// talk about the resulting configuration rather than the change list.
fn applied(
    mut entries: BTreeMap<String, ReplicaEntry>,
    changes: Vec<Change>,
) -> BTreeMap<String, ReplicaEntry> {
    for change in changes {
        match change {
            Change::Set(entry) => {
                entries.insert(entry.key(), entry);
            }
            Change::Drop(key) => {
                entries.remove(&key);
            }
        }
    }
    entries
}

fn tags_of(entries: &BTreeMap<String, ReplicaEntry>, identifier: &str) -> Option<Vec<String>> {
    let key = parse(identifier).unwrap().to_string();
    entries.get(&key).map(|entry| entry.tags.clone())
}

fn write(contents: &str) -> tempfile::NamedTempFile {
    use std::io::Write as _;

    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(contents.as_bytes()).expect("write");
    file.flush().expect("flush");
    file
}

#[test]
fn tags_add_to_a_new_entry() {
    let entries = configured(&[]);
    let changes = from_flags("app:chatty", &tags(&["region-1"]), &[], &entries).unwrap();

    let entries = applied(entries, changes);
    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-1"])));
}

#[test]
fn tags_union_into_an_existing_entry() {
    let entries = configured(&[("app:chatty", &["region-1"])]);
    let changes = from_flags(
        "app:chatty",
        &tags(&["region-2", "region-1"]),
        &[],
        &entries,
    )
    .unwrap();

    let entries = applied(entries, changes);
    assert_eq!(
        tags_of(&entries, "app:chatty"),
        Some(tags(&["region-1", "region-2"])),
    );
}

#[test]
fn exclude_removes_only_that_tag() {
    let entries = configured(&[("app:chatty", &["region-1", "region-2"])]);
    let changes = from_flags("app:chatty", &[], &tags(&["region-1"]), &entries).unwrap();

    let entries = applied(entries, changes);
    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-2"])));
}

#[test]
fn excluding_the_last_tag_drops_the_entry() {
    let entries = configured(&[("app:chatty", &["region-1"])]);
    let changes = from_flags("app:chatty", &[], &tags(&["region-1"]), &entries).unwrap();

    let entries = applied(entries, changes);
    assert_eq!(tags_of(&entries, "app:chatty"), None);
    assert!(entries.is_empty());
}

#[test]
fn excluding_from_an_unconfigured_entry_writes_nothing() {
    let entries = configured(&[]);
    let changes = from_flags("app:chatty", &[], &tags(&["region-1"]), &entries).unwrap();

    assert!(changes.is_empty());
}

#[test]
fn no_flags_is_a_read_not_a_write() {
    let entries = configured(&[("app:chatty", &["region-1"])]);
    let changes = from_flags("app:chatty", &[], &[], &entries).unwrap();

    assert!(changes.is_empty());
}

#[test]
fn an_srn_and_its_uuid_are_the_same_entry() {
    let entries = configured(&[("chatty/server", &["region-1"])]);
    let uuid = parse("chatty/server").unwrap().to_string();

    let changes = from_flags(&uuid, &tags(&["region-2"]), &[], &entries).unwrap();
    let entries = applied(entries, changes);

    assert_eq!(entries.len(), 1);
    assert_eq!(
        tags_of(&entries, "chatty/server"),
        Some(tags(&["region-1", "region-2"])),
    );
}

#[test]
fn an_unparseable_identifier_is_rejected() {
    let entries = configured(&[]);
    let err = from_flags("chatty/", &tags(&["region-1"]), &[], &entries).unwrap_err();

    assert!(
        err.to_string().contains("invalid identifier 'chatty/'"),
        "{err}",
    );
}

#[test]
fn a_system_tag_pins_an_entry_to_a_runtime() {
    let entries = configured(&[]);
    let changes = from_flags("app:chatty", &tags(&["@a0b1c2"]), &[], &entries).unwrap();

    let entries = applied(entries, changes);
    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["@a0b1c2"])));
}

#[test]
fn a_system_tag_can_be_excluded_again() {
    let entries = configured(&[("app:chatty", &["@a0b1c2", "region-1"])]);
    let changes = from_flags("app:chatty", &[], &tags(&["@a0b1c2"]), &entries).unwrap();

    let entries = applied(entries, changes);
    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-1"])));
}

#[test]
fn a_file_replaces_the_tags_it_lists() {
    let entries = configured(&[("app:chatty", &["region-1", "region-2"])]);
    let file = write("app:chatty: [region-3]\n");

    let changes = from_file(file.path(), &entries, false).unwrap();
    let entries = applied(entries, changes);

    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-3"])));
}

#[test]
fn a_json_file_parses_too() {
    // The documented example is JSON, which is valid YAML.
    let entries = configured(&[]);
    let file = write("{\n  \"app:chatty\": [\"region-1\"],\n  \"chatty\": [\"region-2\"]\n}\n");

    let changes = from_file(file.path(), &entries, false).unwrap();
    let entries = applied(entries, changes);

    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-1"])));
    assert_eq!(tags_of(&entries, "chatty"), Some(tags(&["region-2"])));
}

#[test]
fn an_empty_list_in_a_file_drops_the_entry() {
    let entries = configured(&[("app:chatty", &["region-1"])]);
    let file = write("app:chatty: []\n");

    let changes = from_file(file.path(), &entries, false).unwrap();
    let entries = applied(entries, changes);

    assert!(entries.is_empty());
}

#[test]
fn a_file_leaves_entries_it_does_not_mention() {
    let entries = configured(&[("app:chatty", &["region-1"]), ("app:other", &["region-9"])]);
    let file = write("app:chatty: [region-2]\n");

    let changes = from_file(file.path(), &entries, false).unwrap();
    let entries = applied(entries, changes);

    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-2"])));
    assert_eq!(tags_of(&entries, "app:other"), Some(tags(&["region-9"])));
}

#[test]
fn prune_drops_entries_the_file_does_not_mention() {
    let entries = configured(&[("app:chatty", &["region-1"]), ("app:other", &["region-9"])]);
    let file = write("app:chatty: [region-2]\n");

    let changes = from_file(file.path(), &entries, true).unwrap();
    let entries = applied(entries, changes);

    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["region-2"])));
    assert_eq!(tags_of(&entries, "app:other"), None);
}

#[test]
fn a_file_may_use_system_tags() {
    let entries = configured(&[]);
    let file = write("app:chatty: ['@a0b1c2']\n");

    let changes = from_file(file.path(), &entries, false).unwrap();
    let entries = applied(entries, changes);

    assert_eq!(tags_of(&entries, "app:chatty"), Some(tags(&["@a0b1c2"])));
}

#[test]
fn a_file_identifier_that_does_not_parse_is_rejected() {
    let entries = configured(&[]);
    let file = write("chatty/: [region-1]\n");

    let err = from_file(file.path(), &entries, false).unwrap_err();
    assert!(err.to_string().contains("invalid identifier"), "{err}");
}

#[test]
fn an_srn_entry_renders_under_the_name_it_was_written_with() {
    let entries = configured(&[("chatty/server", &["region-1"])]);
    let out = render(&entries, &[], false);

    assert!(out.contains("chatty/server"), "{out}");
    assert!(out.contains("region-1"), "{out}");
}

#[test]
fn a_uuid_entry_renders_as_its_uuid() {
    let uuid = parse("chatty/server").unwrap().to_string();
    let entries = configured(&[(&uuid, &["region-1"])]);
    let out = render(&entries, &[], false);

    assert!(out.contains(&uuid), "{out}");
}

#[test]
fn an_empty_configuration_says_so() {
    assert_eq!(
        render(&configured(&[]), &[], false),
        "no replication sets configured\n",
    );
}

fn custody(node: u8) -> CustodyRow {
    CustodyRow::new(models::Scope::new("tele", "telemetry", "p"), [node; 16])
}

#[test]
fn a_custodian_renders_as_provisional() {
    // No configured entry: the custody row alone puts the scope on the board,
    // marked as provisional so an operator sees intent has diverged.
    let out = render(&configured(&[]), &[custody(1)], false);

    assert!(out.contains("scope:tele/telemetry/p"), "{out}");
    assert!(out.contains("provisional"), "{out}");
}

#[test]
fn a_custodian_joins_its_configured_entry_on_one_row() {
    let entries = configured(&[("scope:tele/telemetry/p", &["region-1"])]);
    let out = render(&entries, &[custody(1)], false);

    let row = out
        .lines()
        .find(|line| line.contains("tele/telemetry/p"))
        .expect("the scope should render");
    assert!(row.contains("region-1"), "{out}");
    assert!(row.contains("provisional"), "{out}");
    assert_eq!(
        out.lines()
            .filter(|l| l.contains("tele/telemetry/p"))
            .count(),
        1,
        "configured tags and custody must share one row: {out}",
    );
}
