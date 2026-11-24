//! A fingerprint of a whole instance tree: its names, classes, properties and
//! shape, but not the IDs Rojo gives instances, which differ every run.
//!
//! `rojo build` writes it into the places it builds and `rojo serve` reports it
//! for its tree, so the plugin can tell when Studio has a place open that is
//! exactly what `rojo serve` would sync. It then skips downloading and diffing
//! every property, which is most of the time a large project takes to connect.

use rbx_dom_weak::types::{Ref, Variant};

use crate::snapshot::RojoTree;

/// Attribute on a built place's Workspace holding the fingerprint of the tree
/// it was built from, written before any of Rojo's other attributes.
pub const TREE_FINGERPRINT_ATTRIBUTE: &str = "__Rojo_TreeFingerprint";

pub fn tree_fingerprint(tree: &RojoTree) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_instance(tree, tree.get_root_id(), &mut hasher);
    hasher.finalize().to_hex()[..32].to_owned()
}

fn hash_instance(tree: &RojoTree, id: Ref, hasher: &mut blake3::Hasher) {
    let Some(instance) = tree.get_instance(id) else {
        return;
    };

    write_str(hasher, instance.name());
    write_str(hasher, instance.class_name().as_str());

    // Property maps are unordered, so sort them for a stable result
    let mut properties: Vec<_> = instance.properties().iter().collect();
    properties.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
    hasher.update(&(properties.len() as u64).to_le_bytes());
    for (key, value) in properties {
        write_str(hasher, key.as_str());
        match value {
            // IDs are new every run, so a reference counts by where its target is
            Variant::Ref(target) => write_str(hasher, &path_of(tree, *target)),
            other => match rmp_serde::to_vec(other) {
                Ok(bytes) => {
                    hasher.update(&(bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                }
                Err(_) => write_str(hasher, "<unserializable>"),
            },
        }
    }

    let children = instance.children();
    hasher.update(&(children.len() as u64).to_le_bytes());
    for &child in children {
        hash_instance(tree, child, hasher);
    }
}

fn write_str(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

/// The names from the root down to `target`, with each one's position among
/// its siblings, since names alone can repeat.
fn path_of(tree: &RojoTree, target: Ref) -> String {
    let mut parts = Vec::new();
    let mut current = target;
    while let Some(instance) = tree.get_instance(current) {
        let parent = instance.parent();
        let index = tree
            .get_instance(parent)
            .and_then(|p| p.children().iter().position(|&c| c == current))
            .unwrap_or(0);
        parts.push(format!("{}#{index}", instance.name()));
        current = parent;
    }
    if parts.is_empty() {
        return "<none>".to_owned();
    }
    parts.reverse();
    parts.join("/")
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::snapshot::InstanceSnapshot;

    fn tree(value: &str, child_name: &str) -> RojoTree {
        let snapshot = InstanceSnapshot::new()
            .name("Root")
            .class_name("Folder")
            .children(vec![InstanceSnapshot::new()
                .name(child_name)
                .class_name("StringValue")
                .property("Value", value)]);
        RojoTree::new(snapshot)
    }

    #[test]
    fn same_tree_same_fingerprint() {
        // Each tree gets new IDs, which must not change the fingerprint
        assert_eq!(
            tree_fingerprint(&tree("a", "Child")),
            tree_fingerprint(&tree("a", "Child"))
        );
    }

    #[test]
    fn changes_change_the_fingerprint() {
        let base = tree_fingerprint(&tree("a", "Child"));
        assert_ne!(base, tree_fingerprint(&tree("b", "Child")));
        assert_ne!(base, tree_fingerprint(&tree("a", "Other")));
    }
}
