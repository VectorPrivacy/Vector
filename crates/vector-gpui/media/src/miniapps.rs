//! The account's Iroh node, which calls share with Mini Apps.

pub mod realtime {
    use anyhow::{anyhow, Result};
    use iroh::EndpointAddr;
    use std::sync::Arc;

    pub(crate) async fn mesh() -> Result<Arc<vector_core::xdc::mesh::Mesh>, String> {
        vector_core::xdc::mesh::mesh().await
    }

    pub fn encode_node_addr(addr: &EndpointAddr) -> Result<String> {
        vector_core::xdc::wire::encode_node_addr(addr).map_err(|e| anyhow!(e))
    }

    /// Relay paths only: the sender chose every address in it, and dialling a direct one shows them our IP.
    pub fn decode_node_addr(s: &str) -> Result<EndpointAddr> {
        vector_core::xdc::wire::decode_node_addr(s).map_err(|e| anyhow!(e))
    }
}
