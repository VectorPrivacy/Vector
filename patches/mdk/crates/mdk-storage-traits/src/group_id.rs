//! GroupId wrapper around OpenMLS GroupId

use serde::{Deserialize, Serialize};

/// MDK Group ID wrapper around OpenMLS GroupId
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupId(openmls::group::GroupId);

impl GroupId {
    /// Create a new GroupId from a byte slice
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self(openmls::group::GroupId::from_slice(bytes))
    }

    /// Convert the GroupId to a byte slice
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Convert the GroupId to a byte vector
    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    /// Get the underlying OpenMLS GroupId (internal use)
    pub fn inner(&self) -> &openmls::group::GroupId {
        &self.0
    }
}

impl From<openmls::group::GroupId> for GroupId {
    fn from(id: openmls::group::GroupId) -> Self {
        Self(id)
    }
}

impl From<&openmls::group::GroupId> for GroupId {
    fn from(id: &openmls::group::GroupId) -> Self {
        Self(id.clone())
    }
}

impl From<GroupId> for openmls::group::GroupId {
    fn from(id: GroupId) -> Self {
        id.0
    }
}

impl From<&GroupId> for openmls::group::GroupId {
    fn from(id: &GroupId) -> Self {
        id.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn test_group_id_from_slice() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let group_id = GroupId::from_slice(&bytes);
        assert_eq!(group_id.as_slice(), &bytes);
    }

    #[test]
    fn test_group_id_to_vec() {
        let bytes = vec![10u8, 20, 30, 40];
        let group_id = GroupId::from_slice(&bytes);
        assert_eq!(group_id.to_vec(), bytes);
    }

    #[test]
    fn test_group_id_inner() {
        let bytes = [1u8, 2, 3, 4];
        let group_id = GroupId::from_slice(&bytes);
        let inner = group_id.inner();
        assert_eq!(inner.as_slice(), &bytes);
    }

    #[test]
    fn test_group_id_equality() {
        let bytes = [1u8, 2, 3, 4];
        let id1 = GroupId::from_slice(&bytes);
        let id2 = GroupId::from_slice(&bytes);
        let id3 = GroupId::from_slice(&[5, 6, 7, 8]);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_group_id_clone() {
        let bytes = [1u8, 2, 3, 4];
        let id1 = GroupId::from_slice(&bytes);
        let id2 = id1.clone();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_group_id_debug() {
        let bytes = [1u8, 2, 3, 4];
        let group_id = GroupId::from_slice(&bytes);
        let debug_str = format!("{:?}", group_id);
        assert!(debug_str.contains("GroupId"));
    }

    #[test]
    fn test_group_id_hash() {
        let bytes1 = [1u8, 2, 3, 4];
        let bytes2 = [5u8, 6, 7, 8];
        let id1 = GroupId::from_slice(&bytes1);
        let id2 = GroupId::from_slice(&bytes2);
        let id1_dup = GroupId::from_slice(&bytes1);

        let mut set = HashSet::new();
        set.insert(id1.clone());
        set.insert(id2);

        assert!(set.contains(&id1));
        assert!(set.contains(&id1_dup));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_group_id_ordering() {
        let id1 = GroupId::from_slice(&[1u8, 2, 3, 4]);
        let id2 = GroupId::from_slice(&[1u8, 2, 3, 5]);
        let id3 = GroupId::from_slice(&[1u8, 2, 3, 4]);

        assert!(id1 < id2);
        assert!(id2 > id1);
        assert!(id1 <= id3);
        assert!(id1 >= id3);
    }

    #[test]
    fn test_group_id_from_openmls() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let openmls_id = openmls::group::GroupId::from_slice(&bytes);
        let group_id: GroupId = openmls_id.clone().into();
        assert_eq!(group_id.as_slice(), &bytes);

        // Test From<&openmls::group::GroupId>
        let group_id_ref: GroupId = (&openmls_id).into();
        assert_eq!(group_id_ref.as_slice(), &bytes);
    }

    #[test]
    fn test_group_id_to_openmls() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let group_id = GroupId::from_slice(&bytes);

        // Test From<GroupId>
        let openmls_id: openmls::group::GroupId = group_id.clone().into();
        assert_eq!(openmls_id.as_slice(), &bytes);

        // Test From<&GroupId>
        let openmls_id_ref: openmls::group::GroupId = (&group_id).into();
        assert_eq!(openmls_id_ref.as_slice(), &bytes);
    }

    #[test]
    fn test_group_id_serialization() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let group_id = GroupId::from_slice(&bytes);

        // Serialize
        let json = serde_json::to_string(&group_id).expect("Failed to serialize");

        // Deserialize
        let deserialized: GroupId = serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(group_id, deserialized);
    }
}
