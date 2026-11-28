jpeg-decoder 0.3.2 (image-rs, MIT OR Apache-2.0), changed for the way Vector decodes photos:
at 1/8, 1/4 or 1/2 scale, on the thread that asked.

- Decoding runs on the calling thread: no worker threads or channels per image.
- No allocation per MCU row: coefficient rows and upsampling lines are reused.
- Coefficients are held only for the corner the scaled IDCT reads (n×n of each 8×8 block),
  with a nonzero bit for the rest where a refinement scan needs it. Progressive scans that
  cannot reach that corner are skipped to the next marker without Huffman decoding.
- `Decoder::decode_rows` hands out each converted row instead of building the whole image.
- Huffman refill takes eight bytes at once when none of them is 0xFF.

Output is byte-identical to stock jpeg-decoder 0.3.2 on the same platform: `tests/stock.rs`
compares both across a corpus at every scale (`JPEG_CORPUS=<dir>` adds more files) and on
mutated inputs. `tests/corpus/make.py` writes the corpus.

`cargo bench -p vector-jpeg --bench scaled -- <dirs or files>` times both at 1/8, 1/4 and 1/2 and
counts what each allocates.
