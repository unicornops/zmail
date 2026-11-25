//! The names JMAP mailboxes are stored under (#512).
//!
//! JMAP knows a mailbox by its id, and RFC 8621 section 2 asks only that
//! a mailbox's name be unique among its siblings: `Clients/Invoices` and
//! `Suppliers/Invoices` are both named `Invoices`. The store tells
//! mailboxes apart by name, so each JMAP mailbox is stored under its
//! path, the names from the top down to it joined by [`DELIMITER`], as
//! the IMAP adapter stores full IMAP names and the Proton adapter
//! Bridge's paths (`Folders/Work`). Its parent is stored by path too.
//!
//! A path changes when the mailbox or one of its parents is renamed or
//! moved on the server. The ids recorded at the last sync say which
//! stored mailbox that was ([`renames`]), and the store renames it in
//! place, conversations and queued changes and all. The same renames
//! move mailboxes stored under their own names before #512 to their
//! paths.

use std::collections::{BTreeMap, HashMap, HashSet};

use zmail_core::{MailboxRename, MailboxSnapshot};

use crate::JmapMailbox;

/// What separates the names in a stored path, as in Bridge's and most
/// IMAP servers' mailbox names.
pub(crate) const DELIMITER: &str = "/";

/// A mailbox as `Mailbox/get` lists it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Listed<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) parent_id: Option<&'a str>,
}

/// Where a mailbox is stored: its path, and its parent's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placed {
    pub(crate) path: String,
    pub(crate) parent: Option<String>,
}

/// The path of every listed mailbox, by id.
///
/// A mailbox whose parent is not listed is stored at the top, as is each
/// mailbox of a parent cycle (which no sane server sends). Two mailboxes
/// can still arrive at one path when a name holds the delimiter
/// (`A/B` at the top and `B` under `A`): the one nearer the top, then
/// the one with the lower id, keeps the path, and the others have their
/// id added to their own name (`B (mb-7)`), so every path is distinct
/// and stays the same from sync to sync.
pub(crate) fn mailbox_paths(listed: &[Listed<'_>]) -> HashMap<String, Placed> {
    let by_id = listed
        .iter()
        .map(|mailbox| (mailbox.id, *mailbox))
        .collect::<HashMap<_, _>>();
    let listed_parent = |id: &str| {
        by_id
            .get(id)
            .and_then(|mailbox| mailbox.parent_id)
            .filter(|parent| by_id.contains_key(parent))
    };
    let in_cycle = |id: &str| {
        let mut seen = HashSet::new();
        let mut current = listed_parent(id);
        while let Some(parent) = current {
            if parent == id {
                return true;
            }
            if !seen.insert(parent) {
                return false;
            }
            current = listed_parent(parent);
        }
        false
    };
    let parents = listed
        .iter()
        .map(|mailbox| {
            let parent = (!in_cycle(mailbox.id))
                .then(|| listed_parent(mailbox.id))
                .flatten();
            (mailbox.id, parent)
        })
        .collect::<HashMap<_, _>>();
    // With the cycles cut, every chain of parents ends at the top.
    let depth = |id: &str| {
        let mut depth = 0_usize;
        let mut current = parents.get(id).copied().flatten();
        while let Some(parent) = current {
            depth += 1;
            current = parents.get(parent).copied().flatten();
        }
        depth
    };

    let mut order = listed
        .iter()
        .map(|mailbox| (depth(mailbox.id), mailbox.id))
        .collect::<Vec<_>>();
    order.sort_unstable();
    order.dedup();

    let mut placed = HashMap::<String, Placed>::with_capacity(order.len());
    let mut taken = HashSet::<String>::with_capacity(order.len());
    for (_, id) in order {
        let name = by_id[id].name;
        let parent = parents[id].and_then(|parent| placed.get(parent).map(|p| p.path.clone()));
        let joined = |name: &str| match &parent {
            Some(parent) => format!("{parent}{DELIMITER}{name}"),
            None => name.to_owned(),
        };
        let mut path = joined(name);
        let mut suffix = id.to_owned();
        while taken.contains(&path) {
            path = joined(&format!("{name} ({suffix})"));
            suffix.push('\'');
        }
        taken.insert(path.clone());
        placed.insert(id.to_owned(), Placed { path, parent });
    }
    placed
}

/// The stored mailboxes whose names change in this sync, renamed in
/// place by the store before the snapshot is written.
///
/// `previous` is the stored name of each mailbox id at the last sync
/// (`None` for a checkpoint from before the ids were recorded: those
/// mailboxes are replaced, not renamed). A mailbox is renamed when it
/// is stored under its previous name, and no other id had that name:
/// two that shared one before #512 were stored as one, and the full sync
/// rebuilds them.
pub(crate) fn renames(
    previous: Option<&BTreeMap<String, String>>,
    details: &[JmapMailbox],
    existing: &[MailboxSnapshot],
) -> Vec<MailboxRename> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let mut holders = HashMap::<&str, usize>::new();
    for name in previous.values() {
        *holders.entry(name.as_str()).or_default() += 1;
    }
    let stored = existing
        .iter()
        .map(|mailbox| mailbox.name.as_str())
        .collect::<HashSet<_>>();
    details
        .iter()
        .filter_map(|detail| {
            let from = previous.get(&detail.id)?;
            (from != &detail.name
                && holders.get(from.as_str()) == Some(&1)
                && stored.contains(from.as_str()))
            .then(|| MailboxRename {
                from: from.clone(),
                to: detail.name.clone(),
                parent: detail.parent.clone(),
            })
        })
        .collect()
}

/// Whether the mirror stores every mailbox where `details` puts it, so
/// it can be handed back as it is when nothing changed on the server.
pub(crate) fn mirror_matches(existing: &[MailboxSnapshot], details: &[JmapMailbox]) -> bool {
    let stored = existing
        .iter()
        .map(|mailbox| {
            (
                mailbox.name.as_str(),
                mailbox.parent.as_deref(),
                mailbox.delimiter.as_deref(),
            )
        })
        .collect::<HashSet<_>>();
    let wanted = details
        .iter()
        .map(|detail| {
            (
                detail.name.as_str(),
                detail.parent.as_deref(),
                Some(DELIMITER),
            )
        })
        .collect::<HashSet<_>>();
    stored == wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed<'a>(entries: &[(&'a str, &'a str, Option<&'a str>)]) -> Vec<Listed<'a>> {
        entries
            .iter()
            .map(|(id, name, parent_id)| Listed {
                id,
                name,
                parent_id: *parent_id,
            })
            .collect()
    }

    fn paths(entries: &[(&str, &str, Option<&str>)]) -> BTreeMap<String, (String, Option<String>)> {
        mailbox_paths(&listed(entries))
            .into_iter()
            .map(|(id, placed)| (id, (placed.path, placed.parent)))
            .collect()
    }

    fn at(path: &str, parent: Option<&str>) -> (String, Option<String>) {
        (path.to_owned(), parent.map(str::to_owned))
    }

    #[test]
    fn paths_join_the_names_from_the_top() {
        let paths = paths(&[
            ("c", "Clients", None),
            ("ci", "Invoices", Some("c")),
            ("s", "Suppliers", None),
            ("si", "Invoices", Some("s")),
            ("ca", "Acme", Some("c")),
            ("cai", "Invoices", Some("ca")),
        ]);
        assert_eq!(paths["c"], at("Clients", None));
        assert_eq!(paths["ci"], at("Clients/Invoices", Some("Clients")));
        assert_eq!(paths["si"], at("Suppliers/Invoices", Some("Suppliers")));
        assert_eq!(
            paths["cai"],
            at("Clients/Acme/Invoices", Some("Clients/Acme"))
        );
    }

    #[test]
    fn a_parent_the_server_did_not_list_puts_the_mailbox_at_the_top() {
        let paths = paths(&[("o", "Orphan", Some("gone")), ("k", "Kid", Some("o"))]);
        assert_eq!(paths["o"], at("Orphan", None));
        assert_eq!(paths["k"], at("Orphan/Kid", Some("Orphan")));
    }

    #[test]
    fn a_parent_cycle_is_cut_at_the_top() {
        let paths = paths(&[
            ("a", "A", Some("b")),
            ("b", "B", Some("a")),
            ("k", "Kid", Some("a")),
            ("s", "Selfish", Some("s")),
        ]);
        assert_eq!(paths["a"], at("A", None));
        assert_eq!(paths["b"], at("B", None));
        assert_eq!(paths["k"], at("A/Kid", Some("A")));
        assert_eq!(paths["s"], at("Selfish", None));
    }

    #[test]
    fn a_name_holding_the_delimiter_cannot_take_another_mailboxs_path() {
        let entries = [
            ("x", "B", Some("a")),
            ("a", "A", None),
            ("top", "A/B", None),
            ("y", "C", Some("x")),
        ];
        let paths = paths(&entries);
        assert_eq!(paths["top"], at("A/B", None));
        assert_eq!(paths["x"], at("A/B (x)", Some("A")));
        assert_eq!(paths["y"], at("A/B (x)/C", Some("A/B (x)")));
        // The same answer in another order gives the same paths.
        let mut reversed = entries;
        reversed.reverse();
        assert_eq!(super::tests::paths(&reversed), paths);
    }

    fn detail(id: &str, name: &str, parent: Option<&str>) -> JmapMailbox {
        JmapMailbox {
            id: id.to_owned(),
            name: name.to_owned(),
            own_name: name.rsplit(DELIMITER).next().unwrap_or(name).to_owned(),
            role: None,
            sort_order: 0,
            parent: parent.map(str::to_owned),
        }
    }

    fn snapshot(name: &str) -> MailboxSnapshot {
        MailboxSnapshot {
            name: name.to_owned(),
            ..MailboxSnapshot::default()
        }
    }

    fn ids(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .collect()
    }

    #[test]
    fn mailboxes_whose_paths_changed_are_renamed() {
        let details = [
            detail("c", "Customers", None),
            detail("ci", "Customers/Invoices", Some("Customers")),
            detail("n", "Notes", None),
        ];
        let previous = ids(&[("c", "Clients"), ("ci", "Clients/Invoices"), ("n", "Notes")]);
        let existing = [
            snapshot("Clients"),
            snapshot("Clients/Invoices"),
            snapshot("Notes"),
        ];
        assert_eq!(
            renames(Some(&previous), &details, &existing),
            [
                MailboxRename {
                    from: "Clients".to_owned(),
                    to: "Customers".to_owned(),
                    parent: None,
                },
                MailboxRename {
                    from: "Clients/Invoices".to_owned(),
                    to: "Customers/Invoices".to_owned(),
                    parent: Some("Customers".to_owned()),
                },
            ]
        );
        assert!(renames(None, &details, &existing).is_empty());
    }

    #[test]
    fn a_name_two_mailboxes_shared_is_not_renamed() {
        let details = [
            detail("ci", "Clients/Invoices", Some("Clients")),
            detail("si", "Suppliers/Invoices", Some("Suppliers")),
            detail("w", "Clients/Work", Some("Clients")),
        ];
        // Before #512 both were stored as `Invoices`; `Work` never was.
        let previous = ids(&[("ci", "Invoices"), ("si", "Invoices"), ("w", "Work")]);
        let existing = [snapshot("Invoices")];
        assert!(renames(Some(&previous), &details, &existing).is_empty());
    }

    #[test]
    fn a_mirror_matches_only_when_stored_by_path() {
        let details = [
            detail("p", "Projects", None),
            detail("w", "Projects/Website", Some("Projects")),
        ];
        let stored = |name: &str, parent: Option<&str>, delimiter: Option<&str>| MailboxSnapshot {
            name: name.to_owned(),
            parent: parent.map(str::to_owned),
            delimiter: delimiter.map(str::to_owned),
            ..MailboxSnapshot::default()
        };
        let by_path = [
            stored("Projects", None, Some("/")),
            stored("Projects/Website", Some("Projects"), Some("/")),
        ];
        assert!(mirror_matches(&by_path, &details));
        // Stored before #512: the unchanged mirror is not handed back.
        let by_own_name = [
            stored("Projects", None, None),
            stored("Website", Some("Projects"), None),
        ];
        assert!(!mirror_matches(&by_own_name, &details));
    }
}
