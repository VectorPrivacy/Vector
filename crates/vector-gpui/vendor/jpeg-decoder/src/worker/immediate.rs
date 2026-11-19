use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use crate::decoder::MAX_COMPONENTS;
use crate::idct::dequantize_and_idct_block;
use crate::alloc::sync::Arc;
use crate::parser::Component;
use crate::upsampler::Lines;
use super::RowData;

/// Dequantizes and inverse-transforms rows of blocks on the calling thread, one plane per component.
pub struct ImmediateWorker {
    offsets: [usize; MAX_COMPONENTS],
    // The line `results` starts at; past 0 only for a component started with `start_lines`.
    first: [usize; MAX_COMPONENTS],
    windowed: [bool; MAX_COMPONENTS],
    results: Vec<Vec<u8>>,
    components: Vec<Option<Component>>,
    quantization_tables: Vec<Option<Arc<[u16; 64]>>>,
}

impl Default for ImmediateWorker {
    fn default() -> Self {
        ImmediateWorker {
            offsets: [0; MAX_COMPONENTS],
            first: [0; MAX_COMPONENTS],
            windowed: [false; MAX_COMPONENTS],
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
        self.first[data.index] = 0;
        self.windowed[data.index] = false;
    }

    /// Like `start`, but only the lines not yet passed to `release` are kept.
    pub fn start_lines(&mut self, data: RowData) {
        assert!(self.results[data.index].is_empty());

        let c = &data.component;
        let lines = (usize::from(c.vertical_sampling_factor) * c.dct_scale + 4) * 2;
        self.offsets[data.index] = 0;
        self.first[data.index] = 0;
        self.windowed[data.index] = true;
        self.results[data.index].resize(usize::from(c.block_size.width) * c.dct_scale * lines, 0u8);
        self.components[data.index] = Some(data.component);
        self.quantization_tables[data.index] = Some(data.quantization_table);
    }

    fn stride(&self, index: usize) -> usize {
        let component = self.components[index].as_ref().unwrap();
        usize::from(component.block_size.width) * component.dct_scale
    }

    /// The lines decoded so far that haven't been released.
    pub fn lines(&self, index: usize) -> Lines<'_> {
        Lines { data: &self.results[index][..self.offsets[index]], stride: self.stride(index), first: self.first[index] }
    }

    /// How many lines have been decoded, released ones included.
    pub fn filled(&self, index: usize) -> usize {
        self.first[index] + self.offsets[index] / self.stride(index)
    }

    /// Drops the lines before `line`.
    pub fn release(&mut self, index: usize, line: usize) {
        let stride = self.stride(index);
        let drop = ((line.max(self.first[index]) - self.first[index]) * stride).min(self.offsets[index]);
        self.results[index].copy_within(drop..self.offsets[index], 0);
        self.offsets[index] -= drop;
        self.first[index] += drop / stride;
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
        let end = self.offsets[index] + block_count * per_block;
        if self.windowed[index] && self.results[index].len() < end {
            self.results[index].resize(end, 0);
        }

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
        self.windowed[index] = false;
        mem::take(&mut self.results[index])
    }
}
