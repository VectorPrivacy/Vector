use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use crate::decoder::MAX_COMPONENTS;
use crate::idct::dequantize_and_idct_block;
use crate::alloc::sync::Arc;
use crate::parser::Component;
use super::RowData;

/// Dequantizes and inverse-transforms rows of blocks on the calling thread, one plane per component.
pub struct ImmediateWorker {
    offsets: [usize; MAX_COMPONENTS],
    results: Vec<Vec<u8>>,
    components: Vec<Option<Component>>,
    quantization_tables: Vec<Option<Arc<[u16; 64]>>>,
}

impl Default for ImmediateWorker {
    fn default() -> Self {
        ImmediateWorker {
            offsets: [0; MAX_COMPONENTS],
            results: vec![Vec::new(); MAX_COMPONENTS],
            components: vec![None; MAX_COMPONENTS],
            quantization_tables: vec![None; MAX_COMPONENTS],
        }
    }
}

impl ImmediateWorker {
    pub fn start(&mut self, data: RowData) {
        assert!(self.results[data.index].is_empty());

        self.offsets[data.index] = 0;
        self.results[data.index].resize(data.component.block_size.width as usize * data.component.block_size.height as usize * data.component.dct_scale * data.component.dct_scale, 0u8);
        self.components[data.index] = Some(data.component);
        self.quantization_tables[data.index] = Some(data.quantization_table);
    }

    /// A row of blocks, each the `dct_scale`×`dct_scale` corner of its coefficients in natural order.
    pub fn append_row(&mut self, index: usize, data: &[i16]) {
        let component = self.components[index].as_ref().unwrap();
        let quantization_table = self.quantization_tables[index].as_ref().unwrap();
        let block_count = component.block_size.width as usize * component.vertical_sampling_factor as usize;
        let n = component.dct_scale;
        let per_block = n * n;
        let line_stride = component.block_size.width as usize * n;

        assert_eq!(data.len(), block_count * per_block);

        let mut coefficients = [0i16; 64];
        for (i, corner) in data.chunks_exact(per_block).enumerate() {
            let x = (i % component.block_size.width as usize) * n;
            let y = (i / component.block_size.width as usize) * n;

            if n == 8 {
                coefficients.copy_from_slice(corner);
            } else {
                for (row, values) in corner.chunks_exact(n).enumerate() {
                    coefficients[row * 8..][..n].copy_from_slice(values);
                }
            }
            let output = &mut self.results[index][self.offsets[index] + y * line_stride + x..];

            dequantize_and_idct_block(n, &coefficients, quantization_table, line_stride, output);
        }

        self.offsets[index] += block_count * per_block;
    }

    pub fn get_result(&mut self, index: usize) -> Vec<u8> {
        mem::take(&mut self.results[index])
    }
}
