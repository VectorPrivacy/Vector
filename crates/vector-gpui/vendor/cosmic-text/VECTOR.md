cosmic-text 0.19.0 from crates.io, with one change in `src/font/system.rs`: each face's weight
axis is read from its file once (`wght_axes`), not once per new set of attributes in
`get_font_matches`. Tests, fonts, samples and benches are left out; run them from the
published crate with this `src/` swapped in.
