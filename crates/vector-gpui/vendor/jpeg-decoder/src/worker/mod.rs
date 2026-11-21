mod immediate;

use crate::parser::Component;
use alloc::sync::Arc;

pub use immediate::ImmediateWorker;

pub struct RowData {
    pub index: usize,
    pub component: Component,
    pub quantization_table: Arc<[u16; 64]>,
}
