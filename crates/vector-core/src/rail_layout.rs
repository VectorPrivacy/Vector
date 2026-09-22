//! The community rail's arrangement: an ordered list of communities and the
//! folders that group them, synced across the account's own devices.
//!
//! On the wire it is a [`crate::synced_prefs::Pref::Rail`] document — a
//! self-encrypted kind 30078 with the d tag `vector/rail` — deliberately
//! separate from the Community List, which carries join material. A drag is the
//! most frequent write the UI makes, and the list that holds keys is the last
//! document that should absorb that traffic: it is fragmented against a byte
//! ceiling, and it has already been wedged once by growing past one. Here a
//! failed write costs a session of default ordering and nothing else.
//!
//! ```text
//!   kind:    30078 (APPLICATION_SPECIFIC)
//!   tags:    ["d", "vector/rail"]
//!   content: nip44(self, {"v":1,"nodes":[
//!              {"type":"item","key":"<community-id>"},
//!              {"type":"folder","id":"…","name":"Work","hue":210,"keys":["…","…"]}
//!            ]})
//! ```
//!
//! **Keys are opaque and never dropped.** A community this device has not
//! synced yet keeps its place through every read and republish; rendering
//! simply skips what it cannot resolve. Dropping unknown keys would let the
//! device that knows least flatten everyone else's folders. The single
//! exception is [`RailLayout::remove_key`], called when the user LEAVES a
//! community: without it a later rejoin would silently reappear inside its old
//! folder at its old index.
//!
//! Every operation here is pure, so the arrangement can be tested without a
//! relay and the UI stays a view over it.

use serde::{Deserialize, Serialize};

/// The version this build writes. A document from the future is read, rendered
/// and republished untouched, but never EDITED: an older build cannot know what
//  a newer node type means, and rewriting the list would drop it.
pub const RAIL_VERSION: u32 = 1;

/// Folders per account, and communities per folder. High enough that nobody
/// sane meets them, low enough that a malformed document cannot make the rail
/// unrenderable.
const MAX_FOLDERS: usize = 64;
const MAX_FOLDER_NAME: usize = 48;

/// One row of the rail: a community on its own, or a folder holding several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RailNode {
    Item {
        key: String,
    },
    Folder {
        id: String,
        #[serde(default)]
        name: String,
        /// Degrees on the colour wheel, 0-359. Absent means the neutral grey
        /// a folder nobody coloured wears.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hue: Option<u16>,
        #[serde(default)]
        keys: Vec<String>,
    },
}

impl RailNode {
    pub fn item(key: impl Into<String>) -> Self {
        RailNode::Item { key: key.into() }
    }
}

/// What a drag picked up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RailDrag {
    Item { key: String },
    Folder { id: String },
}

/// A top-level row, named so a drop can say "before this one" without caring
/// whether it is a community or a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RailAnchor {
    Item { key: String },
    Folder { id: String },
}

/// Where a drag let go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "kebab-case")]
pub enum RailDrop {
    /// Insert at the top level, above this row.
    Before { anchor: RailAnchor },
    /// Append at the bottom of the top level.
    End,
    /// Dropped onto a loose community: the two become a new folder.
    Combine { with_key: String },
    /// Into an existing folder, above `before_key` or at its end.
    IntoFolder {
        folder_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_key: Option<String>,
    },
}

/// The stored arrangement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RailLayout {
    #[serde(default = "default_version")]
    pub v: u32,
    #[serde(default)]
    pub nodes: Vec<RailNode>,
}

fn default_version() -> u32 {
    RAIL_VERSION
}

impl Default for RailLayout {
    fn default() -> Self {
        Self { v: RAIL_VERSION, nodes: Vec::new() }
    }
}

impl RailLayout {
    /// Tolerant parse, node by node: a malformed or future node is skipped
    /// rather than costing the whole arrangement. One bad entry must not wedge
    /// a rail the user can otherwise still use.
    pub fn from_json(s: &str) -> Self {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default = "default_version")]
            v: u32,
            #[serde(default)]
            nodes: Vec<serde_json::Value>,
        }
        let Ok(raw) = serde_json::from_str::<Raw>(s) else { return Self::default() };
        Self {
            v: raw.v,
            nodes: raw
                .nodes
                .into_iter()
                .filter_map(|n| serde_json::from_value::<RailNode>(n).ok())
                .collect(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"nodes\":[]}".to_string())
    }

    /// Whether this build may rewrite the document. A newer version can hold
    /// node types this one drops on parse, so editing would publish a lossy
    /// copy over the device that understands it.
    pub fn is_editable(&self) -> bool {
        self.v <= RAIL_VERSION
    }

    /// Every community key in visual order, folders flattened in place.
    pub fn flatten(&self) -> Vec<String> {
        let mut out = Vec::new();
        for node in &self.nodes {
            match node {
                RailNode::Item { key } => out.push(key.clone()),
                RailNode::Folder { keys, .. } => out.extend(keys.iter().cloned()),
            }
        }
        out
    }

    pub fn contains(&self, key: &str) -> bool {
        self.nodes.iter().any(|node| match node {
            RailNode::Item { key: k } => k == key,
            RailNode::Folder { keys, .. } => keys.iter().any(|k| k == key),
        })
    }

    fn folder_mut(&mut self, folder_id: &str) -> Option<&mut RailNode> {
        self.nodes.iter_mut().find(|node| matches!(node, RailNode::Folder { id, .. } if id == folder_id))
    }

    /// One key appears once, a folder id appears once, and a folder holding a
    /// single community becomes that community again — a folder of one is a
    /// box with nothing to group, and dragging the second-to-last member out is
    /// how you say you are done with it.
    pub fn normalize(&mut self) {
        let mut seen_keys = std::collections::HashSet::new();
        let mut seen_folders = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(self.nodes.len());
        for node in std::mem::take(&mut self.nodes) {
            match node {
                RailNode::Item { key } => {
                    if seen_keys.insert(key.clone()) {
                        out.push(RailNode::Item { key });
                    }
                }
                RailNode::Folder { id, name, hue, keys } => {
                    if !seen_folders.insert(id.clone()) {
                        continue;
                    }
                    let keys: Vec<String> =
                        keys.into_iter().filter(|k| seen_keys.insert(k.clone())).collect();
                    match keys.len() {
                        0 => continue,
                        1 => out.push(RailNode::Item { key: keys.into_iter().next().unwrap() }),
                        _ => out.push(RailNode::Folder {
                            id,
                            name: truncate_name(&name),
                            hue: hue.map(|h| h % 360),
                            keys,
                        }),
                    }
                }
            }
        }
        self.nodes = out;
    }

    /// The arrangement as it should paint: the stored one, plus any community
    /// the user has joined but never arranged, appended in the order the caller
    /// discovered them.
    ///
    /// Nothing is written here. A user who has never dragged anything stores no
    /// document at all, and their rail still orders itself.
    pub fn merged_with(&self, live_keys: &[String]) -> RailLayout {
        let mut out = self.clone();
        out.normalize();
        let mut known: std::collections::HashSet<String> = out.flatten().into_iter().collect();
        for key in live_keys {
            if known.insert(key.clone()) {
                out.nodes.push(RailNode::item(key.clone()));
            }
        }
        out
    }

    /// Lift a key out of wherever it sits, without normalizing — the first half
    /// of every move.
    fn detach(&mut self, key: &str) {
        self.nodes.retain_mut(|node| match node {
            RailNode::Item { key: k } => k.as_str() != key,
            RailNode::Folder { keys, .. } => {
                keys.retain(|k| k != key);
                true
            }
        });
    }

    /// Drop a community from the arrangement for good.
    ///
    /// Destructive on purpose, unlike the render-time skip: the layout keeps
    /// keys it cannot resolve forever, so without this a user who left a
    /// community and rejoined it later would find it back inside its old
    /// folder.
    pub fn remove_key(&mut self, key: &str) {
        self.detach(key);
        self.normalize();
    }

    /// Position of a top-level row.
    fn index_of(&self, anchor: &RailAnchor) -> Option<usize> {
        self.nodes.iter().position(|node| match (node, anchor) {
            (RailNode::Item { key }, RailAnchor::Item { key: want }) => key == want,
            (RailNode::Folder { id, .. }, RailAnchor::Folder { id: want }) => id == want,
            _ => false,
        })
    }

    /// Apply a completed drag. `new_folder_id` supplies the id for a folder
    /// born from a combine drop, taken as a parameter so the whole thing stays
    /// pure and testable.
    pub fn apply_drop(
        &mut self,
        source: &RailDrag,
        target: &RailDrop,
        new_folder_id: &str,
    ) -> Result<(), String> {
        if !self.is_editable() {
            return Err("This rail was arranged by a newer version of Vector".to_string());
        }
        match source {
            RailDrag::Folder { id } => self.move_folder(id, target),
            RailDrag::Item { key } => self.move_item(key, target, new_folder_id),
        }?;
        self.normalize();
        Ok(())
    }

    /// A folder only ever moves among the top-level rows: nesting a folder in a
    /// folder is a hierarchy nobody asked the rail to have.
    fn move_folder(&mut self, folder_id: &str, target: &RailDrop) -> Result<(), String> {
        let from = self
            .index_of(&RailAnchor::Folder { id: folder_id.to_string() })
            .ok_or("No such folder")?;
        let insert_at = match target {
            RailDrop::End => self.nodes.len(),
            RailDrop::Before { anchor } => self.index_of(anchor).ok_or("No such row")?,
            // A folder dropped onto a community or into another folder lands
            // above it rather than inside.
            RailDrop::Combine { with_key } => self
                .index_of(&RailAnchor::Item { key: with_key.clone() })
                .ok_or("No such community")?,
            RailDrop::IntoFolder { folder_id: onto, .. } => self
                .index_of(&RailAnchor::Folder { id: onto.clone() })
                .ok_or("No such folder")?,
        };
        let node = self.nodes.remove(from);
        let insert_at = if insert_at > from { insert_at - 1 } else { insert_at };
        self.nodes.insert(insert_at.min(self.nodes.len()), node);
        Ok(())
    }

    fn move_item(&mut self, key: &str, target: &RailDrop, new_folder_id: &str) -> Result<(), String> {
        match target {
            RailDrop::Combine { with_key } => {
                if with_key == key {
                    return Ok(());
                }
                let folders = self
                    .nodes
                    .iter()
                    .filter(|n| matches!(n, RailNode::Folder { .. }))
                    .count();
                if folders >= MAX_FOLDERS {
                    return Err(format!("A rail holds at most {MAX_FOLDERS} folders"));
                }
                let at = self
                    .index_of(&RailAnchor::Item { key: with_key.clone() })
                    .ok_or("No such community")?;
                // Read the id out before detaching: dragging one of the pair
                // onto the other must not lose the target's own position.
                self.detach(key);
                let at = self
                    .index_of(&RailAnchor::Item { key: with_key.clone() })
                    .unwrap_or(at.min(self.nodes.len()));
                self.nodes[at] = RailNode::Folder {
                    id: new_folder_id.to_string(),
                    name: String::new(),
                    hue: None,
                    keys: vec![with_key.clone(), key.to_string()],
                };
                Ok(())
            }
            RailDrop::IntoFolder { folder_id, before_key } => {
                self.folder_mut(folder_id).ok_or("No such folder")?;
                self.detach(key);
                let Some(RailNode::Folder { keys, .. }) = self.folder_mut(folder_id) else {
                    return Err("No such folder".to_string());
                };
                let at = before_key
                    .as_ref()
                    .and_then(|b| keys.iter().position(|k| k == b))
                    .unwrap_or(keys.len());
                keys.insert(at, key.to_string());
                Ok(())
            }
            RailDrop::End => {
                self.detach(key);
                self.nodes.push(RailNode::item(key));
                Ok(())
            }
            RailDrop::Before { anchor } => {
                // Resolved AFTER the detach, because lifting the key out can
                // dissolve a folder above the anchor and shift every index.
                self.detach(key);
                let at = self.index_of(anchor).unwrap_or(self.nodes.len());
                self.nodes.insert(at, RailNode::item(key));
                Ok(())
            }
        }
    }

    /// Name a folder. An empty name is allowed and means "unnamed" — the rail
    /// shows the avatars alone, exactly as it does before anyone names one.
    pub fn rename_folder(&mut self, folder_id: &str, name: &str) -> Result<(), String> {
        if !self.is_editable() {
            return Err("This rail was arranged by a newer version of Vector".to_string());
        }
        let Some(RailNode::Folder { name: slot, .. }) = self.folder_mut(folder_id) else {
            return Err("No such folder".to_string());
        };
        *slot = truncate_name(name);
        Ok(())
    }

    /// Colour a folder, or clear it back to the default grey with `None`.
    pub fn set_folder_hue(&mut self, folder_id: &str, hue: Option<u16>) -> Result<(), String> {
        if !self.is_editable() {
            return Err("This rail was arranged by a newer version of Vector".to_string());
        }
        let Some(RailNode::Folder { hue: slot, .. }) = self.folder_mut(folder_id) else {
            return Err("No such folder".to_string());
        };
        *slot = hue.map(|h| h % 360);
        Ok(())
    }

    /// Unpack a folder, leaving its communities loose and in place.
    pub fn dissolve_folder(&mut self, folder_id: &str) -> Result<(), String> {
        if !self.is_editable() {
            return Err("This rail was arranged by a newer version of Vector".to_string());
        }
        let at = self
            .index_of(&RailAnchor::Folder { id: folder_id.to_string() })
            .ok_or("No such folder")?;
        let RailNode::Folder { keys, .. } = self.nodes.remove(at) else {
            return Err("No such folder".to_string());
        };
        for (offset, key) in keys.into_iter().enumerate() {
            self.nodes.insert(at + offset, RailNode::item(key));
        }
        self.normalize();
        Ok(())
    }

    /// Give every folder born without one a stable id. Ids are minted by the
    /// caller so the pure ops stay free of randomness.
    pub fn assign_folder_ids(&mut self, mut next_id: impl FnMut() -> String) {
        let mut taken: std::collections::HashSet<String> = self
            .nodes
            .iter()
            .filter_map(|n| match n {
                RailNode::Folder { id, .. } if !id.is_empty() => Some(id.clone()),
                _ => None,
            })
            .collect();
        for node in self.nodes.iter_mut() {
            if let RailNode::Folder { id, .. } = node {
                if !id.is_empty() {
                    continue;
                }
                loop {
                    let candidate = next_id();
                    if taken.insert(candidate.clone()) {
                        *id = candidate;
                        break;
                    }
                }
            }
        }
    }
}

/// A fresh folder id. Short and random rather than sequential: two devices can
/// each make a folder before either has seen the other's, and an index would
/// have them collide into one.
pub fn new_folder_id() -> String {
    use rand::Rng;
    let bytes: [u8; 6] = rand::thread_rng().gen();
    crate::simd::hex::bytes_to_hex_string(&bytes)
}

/// Folder names ride in a document with a byte ceiling and are read at a glance
/// anyway, so a pasted essay is cut rather than refused.
fn truncate_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.chars().count() <= MAX_FOLDER_NAME {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_FOLDER_NAME).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(nodes: Vec<RailNode>) -> RailLayout {
        RailLayout { v: RAIL_VERSION, nodes }
    }

    fn folder(id: &str, keys: &[&str]) -> RailNode {
        RailNode::Folder {
            id: id.to_string(),
            name: String::new(),
            hue: None,
            keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    #[test]
    fn a_never_arranged_rail_still_orders_itself() {
        let stored = RailLayout::default();
        let live = vec!["a".to_string(), "b".to_string()];
        assert_eq!(stored.merged_with(&live).flatten(), live);
        assert!(stored.nodes.is_empty(), "merging paints, it does not write");
    }

    #[test]
    fn a_new_community_lands_at_the_end_rather_than_the_top() {
        let stored = layout(vec![RailNode::item("b"), RailNode::item("a")]);
        let merged = stored.merged_with(&["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(merged.flatten(), vec!["b", "a", "c"]);
    }

    #[test]
    fn keys_this_device_cannot_resolve_keep_their_place() {
        // The whole point of the opaque-key rule: a device mid-sync must not be
        // able to flatten the arrangement it has not finished reading.
        let stored = layout(vec![RailNode::item("unsynced"), folder("f1", &["a", "b"])]);
        let merged = stored.merged_with(&["a".to_string(), "b".to_string()]);
        assert_eq!(merged.flatten(), vec!["unsynced", "a", "b"]);
    }

    #[test]
    fn leaving_a_community_forgets_where_it_sat() {
        let mut l = layout(vec![folder("f1", &["a", "b", "c"]), RailNode::item("d")]);
        l.remove_key("b");
        assert_eq!(l.flatten(), vec!["a", "c", "d"]);
        // And rejoining puts it at the end, not back in the folder.
        assert_eq!(l.merged_with(&["a".into(), "b".into(), "c".into(), "d".into()]).flatten(), vec!["a", "c", "d", "b"]);
    }

    #[test]
    fn a_folder_emptied_to_one_becomes_that_community_again() {
        let mut l = layout(vec![folder("f1", &["a", "b"]), RailNode::item("c")]);
        l.apply_drop(&RailDrag::Item { key: "b".into() }, &RailDrop::End, "new").unwrap();
        assert_eq!(l.flatten(), vec!["a", "c", "b"]);
        assert!(
            !l.nodes.iter().any(|n| matches!(n, RailNode::Folder { .. })),
            "a folder of one has nothing left to group"
        );
    }

    #[test]
    fn dropping_one_community_onto_another_makes_a_folder_in_its_place() {
        let mut l = layout(vec![RailNode::item("a"), RailNode::item("b"), RailNode::item("c")]);
        l.apply_drop(&RailDrag::Item { key: "c".into() }, &RailDrop::Combine { with_key: "a".into() }, "f1")
            .unwrap();
        l.assign_folder_ids(|| "f1".to_string());
        assert_eq!(l.flatten(), vec!["a", "c", "b"]);
        match &l.nodes[0] {
            RailNode::Folder { keys, id, .. } => {
                assert_eq!(keys, &vec!["a".to_string(), "c".to_string()]);
                assert_eq!(id, "f1");
            }
            other => panic!("expected a folder at the target's position, got {other:?}"),
        }
    }

    #[test]
    fn a_drop_before_a_row_below_the_source_does_not_overshoot() {
        let mut l = layout(vec![RailNode::item("a"), RailNode::item("b"), RailNode::item("c")]);
        l.apply_drop(
            &RailDrag::Item { key: "a".into() },
            &RailDrop::Before { anchor: RailAnchor::Item { key: "c".into() } },
            "f1",
        )
        .unwrap();
        assert_eq!(l.flatten(), vec!["b", "a", "c"]);
    }

    #[test]
    fn a_folder_moves_among_rows_without_nesting() {
        let mut l = layout(vec![RailNode::item("a"), folder("f1", &["b", "c"]), RailNode::item("d")]);
        l.apply_drop(
            &RailDrag::Folder { id: "f1".into() },
            &RailDrop::Before { anchor: RailAnchor::Item { key: "a".into() } },
            "new",
        )
        .unwrap();
        assert_eq!(l.flatten(), vec!["b", "c", "a", "d"]);
        assert_eq!(l.nodes.len(), 3, "a folder dropped on a row sits beside it, never inside");
    }

    #[test]
    fn dropping_into_a_folder_respects_the_position_it_was_dropped_at() {
        let mut l = layout(vec![folder("f1", &["a", "b"]), RailNode::item("c")]);
        l.apply_drop(
            &RailDrag::Item { key: "c".into() },
            &RailDrop::IntoFolder { folder_id: "f1".into(), before_key: Some("b".into()) },
            "new",
        )
        .unwrap();
        assert_eq!(l.flatten(), vec!["a", "c", "b"]);
    }

    #[test]
    fn dissolving_leaves_the_communities_where_the_folder_was() {
        let mut l = layout(vec![RailNode::item("x"), folder("f1", &["a", "b"]), RailNode::item("y")]);
        l.dissolve_folder("f1").unwrap();
        assert_eq!(l.flatten(), vec!["x", "a", "b", "y"]);
    }

    #[test]
    fn a_duplicate_key_resolves_to_its_first_position() {
        let mut l = layout(vec![RailNode::item("a"), folder("f1", &["a", "b", "c"])]);
        l.normalize();
        assert_eq!(l.flatten(), vec!["a", "b", "c"], "a key belongs in exactly one place");
    }

    #[test]
    fn malformed_nodes_are_skipped_rather_than_costing_the_arrangement() {
        let json = r#"{"v":1,"nodes":[{"type":"item","key":"a"},{"type":"wormhole"},{"type":"item","key":"b"}]}"#;
        assert_eq!(RailLayout::from_json(json).flatten(), vec!["a", "b"]);
        assert!(RailLayout::from_json("not json").nodes.is_empty());
    }

    #[test]
    fn a_document_from_the_future_is_rendered_but_never_rewritten() {
        let mut l = RailLayout { v: RAIL_VERSION + 1, nodes: vec![RailNode::item("a")] };
        assert!(!l.is_editable());
        assert_eq!(l.flatten(), vec!["a"], "it still paints");
        assert!(l.apply_drop(&RailDrag::Item { key: "a".into() }, &RailDrop::End, "f").is_err());
        assert!(l.rename_folder("f1", "Work").is_err());
    }

    #[test]
    fn a_round_trip_through_json_preserves_names_and_hues() {
        let l = layout(vec![RailNode::Folder {
            id: "f1".into(),
            name: "Work".into(),
            hue: Some(210),
            keys: vec!["a".into(), "b".into()],
        }]);
        let back = RailLayout::from_json(&l.to_json());
        assert_eq!(back, l);
    }

    #[test]
    fn an_absent_hue_stays_absent_on_the_wire() {
        let l = layout(vec![folder("f1", &["a", "b"])]);
        assert!(!l.to_json().contains("hue"), "the default colour is not a stored value");
    }

    #[test]
    fn folder_ids_are_minted_only_for_folders_that_lack_one() {
        let mut l = layout(vec![folder("keep", &["a", "b"]), folder("", &["c", "d"])]);
        let mut n = 0;
        l.assign_folder_ids(|| {
            n += 1;
            format!("minted{n}")
        });
        let ids: Vec<_> = l
            .nodes
            .iter()
            .filter_map(|node| match node {
                RailNode::Folder { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["keep", "minted1"]);
    }

    #[test]
    fn a_long_folder_name_is_cut_rather_than_refused() {
        let mut l = layout(vec![folder("f1", &["a", "b"])]);
        l.rename_folder("f1", &"x".repeat(200)).unwrap();
        match &l.nodes[0] {
            RailNode::Folder { name, .. } => assert_eq!(name.chars().count(), MAX_FOLDER_NAME),
            other => panic!("expected a folder, got {other:?}"),
        }
    }
}
