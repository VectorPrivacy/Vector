//! Untrusted SVG, rendered to pixels in Rust: no SVG markup reaches a webview from here.
//!
//! Opened as a document, an SVG runs scripts, fetches, animates and exposes the browser engine's
//! whole SVG implementation; as pixels it is only a picture. Rendering is still attackable by
//! resources alone, so before resvg sees a document it is parsed once, node-capped, and walked the
//! way rendering would expand it (`<use>` clones, pattern paints, marker vertices, masks, clips,
//! filters, offscreen layers) against fixed budgets. That same parsed document is what usvg then
//! converts, so the budget and the renderer can't read two different trees. usvg reads nothing from
//! disk or the network: string hrefs are refused, embedded images are PNG/JPEG by their bytes and
//! dimension-checked from their headers, and compressed SVG is never inflated.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use resvg::{tiny_skia, usvg};
use roxmltree::Node;

/// Largest SVG accepted, in bytes.
pub const MAX_SVG_BYTES: usize = 8 * 1024 * 1024;
/// Longest side a render may have.
pub const MAX_RENDER_DIM: u32 = 4096;

const MAX_XML_NODES: u32 = 200_000;
/// Elements rendering may produce once every reference is expanded.
const EXPANSION_BUDGET: u64 = 100_000;
const MAX_DEPTH: u32 = 256;
/// Offscreen layers stacked at once: opacity, masks, clips, filters and blends each render into one
/// canvas-sized buffer, so a large canvas allows fewer (see `layer_limit`).
const MAX_LAYERS: u32 = 16;
const LAYER_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
/// Filter primitives applied across the whole document.
const FILTER_BUDGET: u64 = 256;
const MAX_EMBEDDED_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_EMBEDDED_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_EMBEDDED_DIM: u32 = 4096;

/// A rendered SVG: straight (non-premultiplied) RGBA, `width * height * 4` bytes.
pub struct Rendered {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Render an untrusted SVG so its longest side is `max_dim` pixels (capped at [`MAX_RENDER_DIM`]).
/// Refuses anything over budget rather than rendering part of it.
pub fn rasterize(bytes: &[u8], max_dim: u32) -> Result<Rendered, String> {
    if bytes.len() > MAX_SVG_BYTES {
        return Err("SVG too large".into());
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Err("compressed SVG refused".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG is not UTF-8".to_string())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // roxmltree refuses entity expansion bombs and never resolves external entities.
    let options = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: MAX_XML_NODES, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options).map_err(|e| format!("SVG parse: {e}"))?;
    if doc.root_element().tag_name().name() != "svg" {
        return Err("not an SVG document".into());
    }
    let max_dim = max_dim.clamp(1, MAX_RENDER_DIM);
    preflight(&doc, layer_limit(max_dim))?;
    if has_text(&doc) {
        return Err("SVG contains text".into());
    }

    let _slot = RenderSlot::take();
    let embedded = Arc::new(AtomicUsize::new(0));
    let opts = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(move |_mime, data, _| embedded_image(data, &embedded)),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_xmltree(&doc, &opts).map_err(|e| format!("SVG convert: {e}"))?;
    render(&tree, max_dim)
}

/// How many canvas-sized layers fit the layer memory at this size (4 at the largest canvas).
fn layer_limit(max_dim: u32) -> u32 {
    let canvas = max_dim as u64 * max_dim as u64 * 4;
    (LAYER_MEMORY_BYTES / canvas.max(1)).clamp(4, MAX_LAYERS as u64) as u32
}

/// Text needs fonts and a shaper, a megabyte of renderer most SVGs never use, so an SVG with any
/// is refused like one over budget. Lettering already drawn as outlines is only paths.
fn has_text(doc: &roxmltree::Document) -> bool {
    doc.descendants()
        .filter(|n| n.tag_name().name() == "text")
        .any(|t| t.descendants().any(|d| d.is_text() && !d.text().unwrap_or("").trim().is_empty()))
}

/// [`rasterize`], encoded as PNG.
pub fn rasterize_png(bytes: &[u8], max_dim: u32) -> Result<Vec<u8>, String> {
    let r = rasterize(bytes, max_dim)?;
    let img = image::RgbaImage::from_raw(r.width, r.height, r.rgba).ok_or("SVG canvas")?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(png)
}

/// Marks an SVG Vector wrote around a render, so a sweep never renders its own output again.
const RASTER_MARKER: &str = "<!-- vector:raster -->";

/// [`rasterize`], wrapped in a minimal SVG Vector writes itself: one `<image>` holding the rendered
/// PNG. For a file whose `.svg` path is already stored and must keep working: the webview then
/// parses markup Vector wrote, never the sender's.
pub fn rasterize_wrapped(bytes: &[u8], max_dim: u32) -> Result<Vec<u8>, String> {
    let png = rasterize_png(bytes, max_dim)?;
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&png))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    Ok(wrap_png(&png, w, h))
}

/// A blank wrapped SVG, for a file that refused to render: it shows nothing rather than the original.
pub fn empty_wrapped() -> Vec<u8> {
    format!(r#"{RASTER_MARKER}<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#).into_bytes()
}

/// Whether this is an SVG [`rasterize_wrapped`] or [`empty_wrapped`] wrote.
pub fn is_raster_wrapped(bytes: &[u8]) -> bool {
    bytes.starts_with(RASTER_MARKER.as_bytes())
}

fn wrap_png(png: &[u8], w: u32, h: u32) -> Vec<u8> {
    format!(
        r#"{RASTER_MARKER}<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><image width="{w}" height="{h}" href="data:image/png;base64,{}"/></svg>"#,
        base64_simd::STANDARD.encode_to_string(png)
    )
    .into_bytes()
}

/// Whether bytes read as an SVG document: an `<svg` tag near the start, past any BOM, XML
/// declaration, comments or doctype. Only a routing hint; [`rasterize`] decides.
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return false;
    }
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('<') && text.contains("<svg")
}

fn render(tree: &usvg::Tree, max_dim: u32) -> Result<Rendered, String> {
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err("SVG has no size".into());
    }
    let scale = max_dim as f32 / w.max(h);
    let pw = ((w * scale).round() as u32).clamp(1, max_dim);
    let ph = ((h * scale).round() as u32).clamp(1, max_dim);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or("SVG canvas")?;
    resvg::render(tree, tiny_skia::Transform::from_scale(pw as f32 / w, ph as f32 / h), &mut pixmap.as_mut());
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(Rendered { width: pw, height: ph, rgba })
}

/// An embedded `data:` image, judged by its bytes rather than its declared type: PNG or JPEG
/// only (never a nested SVG, which would bring its own budget), and dimensions read from the
/// header before resvg decodes it without a limit.
fn embedded_image(data: Arc<Vec<u8>>, total: &AtomicUsize) -> Option<usvg::ImageKind> {
    if data.len() > MAX_EMBEDDED_IMAGE_BYTES
        || total.fetch_add(data.len(), Ordering::Relaxed) + data.len() > MAX_EMBEDDED_TOTAL_BYTES
    {
        return None;
    }
    let png = data.starts_with(b"\x89PNG\r\n\x1a\n");
    if !png && !data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&data[..]))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w > MAX_EMBEDDED_DIM || h > MAX_EMBEDDED_DIM {
        return None;
    }
    Some(if png { usvg::ImageKind::PNG(data) } else { usvg::ImageKind::JPEG(data) })
}

/// At most two renders at once: each is bounded, but a burst of them shouldn't take every core.
struct RenderSlot;

#[cfg(not(target_arch = "wasm32"))]
static SLOTS: (std::sync::Mutex<u8>, std::sync::Condvar) = (std::sync::Mutex::new(0), std::sync::Condvar::new());

impl RenderSlot {
    fn take() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut busy = SLOTS.0.lock().unwrap_or_else(|e| e.into_inner());
            while *busy >= 2 {
                busy = SLOTS.1.wait(busy).unwrap_or_else(|e| e.into_inner());
            }
            *busy += 1;
        }
        RenderSlot
    }
}

impl Drop for RenderSlot {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut busy = SLOTS.0.lock().unwrap_or_else(|e| e.into_inner());
            *busy = busy.saturating_sub(1);
            SLOTS.1.notify_one();
        }
    }
}

// ─── The budget walk ────────────────────────────────────────────────────────

/// Walk the document as rendering would expand it. Every element visited costs one, so the walk
/// itself is bounded by the budget it enforces, however the references are arranged.
fn preflight(doc: &roxmltree::Document, max_layers: u32) -> Result<(), String> {
    // First occurrence wins, as in usvg: a later duplicate id never renders through a reference.
    let mut ids = HashMap::new();
    for node in doc.descendants() {
        if let Some(id) = node.attribute("id") {
            ids.entry(id).or_insert(node);
        }
    }
    let mut walk = Walk { ids, left: EXPANSION_BUDGET, filters_left: FILTER_BUDGET, max_layers, chain: Vec::new() };
    walk.visit(doc.root_element(), Inherited::default(), 0, 0, false)?;
    walk.style_sheets(doc)
}

/// Paint servers and markers a shape takes from its ancestors (both are inherited properties).
#[derive(Clone, Copy, Default)]
struct Inherited<'a, 'i> {
    fill: Option<Node<'a, 'i>>,
    stroke: Option<Node<'a, 'i>>,
    markers: [Option<Node<'a, 'i>>; 3],
}

struct Walk<'a, 'i> {
    ids: HashMap<&'a str, Node<'a, 'i>>,
    left: u64,
    filters_left: u64,
    max_layers: u32,
    /// The references being expanded right now, to refuse a document that reaches itself.
    chain: Vec<roxmltree::NodeId>,
}

const SHAPES: &[&str] = &["path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "text"];
/// Containers that render only through a reference, never where they stand.
const REFERENCED_ONLY: &[&str] = &[
    "defs", "symbol", "pattern", "marker", "mask", "clipPath", "linearGradient", "radialGradient",
    "filter", "style", "script", "title", "desc", "metadata", "foreignObject",
];

impl<'a, 'i> Walk<'a, 'i> {
    fn charge(&mut self, n: u64) -> Result<(), String> {
        if n > self.left {
            return Err("SVG too complex to render".into());
        }
        self.left -= n;
        Ok(())
    }

    fn visit(&mut self, node: Node<'a, 'i>, inh: Inherited<'a, 'i>, depth: u32, layers: u32, by_ref: bool) -> Result<(), String> {
        if !node.is_element() {
            return Ok(());
        }
        self.charge(1)?;
        if depth > MAX_DEPTH {
            return Err("SVG nested too deep".into());
        }
        let name = node.tag_name().name();
        if !by_ref && REFERENCED_ONLY.contains(&name) {
            return Ok(());
        }

        let inh = self.inherit(node, inh);
        let mut layers = layers;
        let isolating = ["mask", "clip-path", "filter"].iter().any(|p| prop(node, p).is_some_and(|v| v.contains("url(")))
            || prop(node, "opacity").is_some_and(|v| v.trim().parse::<f32>().map_or(true, |o| o < 1.0))
            || prop(node, "mix-blend-mode").is_some_and(|v| v.trim() != "normal")
            || prop(node, "isolation").is_some_and(|v| v.trim() == "isolate");
        if isolating {
            layers += 1;
            if layers > self.max_layers {
                return Err("SVG stacks too many layers".into());
            }
        }
        // Not inherited: each renders its target once, for this element.
        for p in ["mask", "clip-path"] {
            if let Some(target) = prop(node, p).and_then(|v| self.url(&v)) {
                self.enter(target, Inherited::default(), depth, layers)?;
            }
        }
        if let Some(filter) = prop(node, "filter").and_then(|v| self.url(&v)) {
            self.filter(filter, depth, layers)?;
        }

        if SHAPES.contains(&name) {
            for paint in [inh.fill, inh.stroke].into_iter().flatten() {
                self.enter(paint, Inherited::default(), depth, layers)?;
            }
            if matches!(name, "path" | "line" | "polyline" | "polygon") {
                let vertices = vertices(node);
                for marker in inh.markers.into_iter().flatten() {
                    let before = self.left;
                    self.enter(marker, Inherited::default(), depth, layers)?;
                    self.charge((before - self.left).saturating_mul(vertices.saturating_sub(1)))?;
                }
            }
        }

        if name == "use" {
            if let Some(target) = self.href(node) {
                self.enter(target, inh, depth, layers)?;
            }
            return Ok(());
        }
        // A referenced container renders its children; `<symbol>` too, through `<use>`.
        for child in node.children() {
            self.visit(child, inh, depth + 1, layers, false)?;
        }
        Ok(())
    }

    /// Render a referenced element once: its own subtree, or a pattern's (template-inherited) tiles.
    fn enter(&mut self, target: Node<'a, 'i>, inh: Inherited<'a, 'i>, depth: u32, layers: u32) -> Result<(), String> {
        if self.chain.contains(&target.id()) {
            return Err("SVG references itself".into());
        }
        self.chain.push(target.id());
        let target = if target.tag_name().name() == "pattern" { self.pattern_content(target) } else { target };
        let result = self.visit(target, inh, depth + 1, layers, true);
        self.chain.pop();
        result
    }

    /// A pattern without children of its own draws its `href` template's.
    fn pattern_content(&self, mut pattern: Node<'a, 'i>) -> Node<'a, 'i> {
        for _ in 0..16 {
            if pattern.children().any(|c| c.is_element()) {
                break;
            }
            match self.href(pattern) {
                Some(next) if next.tag_name().name() == "pattern" => pattern = next,
                _ => break,
            }
        }
        pattern
    }

    fn filter(&mut self, filter: Node<'a, 'i>, depth: u32, layers: u32) -> Result<(), String> {
        for primitive in filter.children().filter(|c| c.is_element()) {
            if self.filters_left == 0 {
                return Err("SVG applies too many filters".into());
            }
            self.filters_left -= 1;
            self.charge(1)?;
            if primitive.tag_name().name() == "feImage" {
                if let Some(target) = self.href(primitive) {
                    self.enter(target, Inherited::default(), depth, layers)?;
                }
            }
        }
        Ok(())
    }

    /// Stylesheets can paint any element, so a reference made there is charged once per shape.
    fn style_sheets(&mut self, doc: &'a roxmltree::Document<'i>) -> Result<(), String> {
        let shapes = doc.descendants().filter(|n| SHAPES.contains(&n.tag_name().name())).count() as u64;
        for sheet in doc.descendants().filter(|n| n.has_tag_name("style")) {
            let Some(css) = sheet.text() else { continue };
            let mut rest = css;
            while let Some(at) = rest.find("url(") {
                rest = &rest[at + 4..];
                let Some(target) = self.url(&format!("url({rest}")) else { continue };
                if !matches!(target.tag_name().name(), "pattern" | "marker" | "mask" | "clipPath" | "filter") {
                    continue;
                }
                let before = self.left;
                if target.tag_name().name() == "filter" {
                    self.filter(target, 0, 0)?;
                } else {
                    self.enter(target, Inherited::default(), 0, 0)?;
                }
                self.charge((before - self.left).saturating_mul(shapes.saturating_sub(1)))?;
            }
        }
        Ok(())
    }

    fn inherit(&self, node: Node<'a, 'i>, mut inh: Inherited<'a, 'i>) -> Inherited<'a, 'i> {
        let paint = |value: Option<String>, current: Option<Node<'a, 'i>>| match value {
            None => current,
            Some(v) if v.trim() == "inherit" => current,
            Some(v) => self.url(&v).filter(|t| t.tag_name().name() == "pattern"),
        };
        inh.fill = paint(prop(node, "fill"), inh.fill);
        inh.stroke = paint(prop(node, "stroke"), inh.stroke);
        let marker = |value: Option<String>, current: Option<Node<'a, 'i>>| match value {
            None => current,
            Some(v) if v.trim() == "inherit" => current,
            Some(v) => self.url(&v).filter(|t| t.tag_name().name() == "marker"),
        };
        if let Some(all) = prop(node, "marker") {
            inh.markers = [marker(Some(all.clone()), None), marker(Some(all.clone()), None), marker(Some(all), None)];
        }
        for (i, p) in ["marker-start", "marker-mid", "marker-end"].into_iter().enumerate() {
            inh.markers[i] = marker(prop(node, p), inh.markers[i]);
        }
        inh
    }

    /// `href` before `xlink:href`, as usvg resolves them.
    fn href(&self, node: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
        let value = node
            .attributes()
            .find(|a| a.name() == "href" && a.namespace().is_none())
            .or_else(|| node.attributes().find(|a| a.name() == "href" && a.namespace() == Some("http://www.w3.org/1999/xlink")))?
            .value();
        self.ids.get(value.trim().strip_prefix('#')?).copied()
    }

    /// The element a `url(#id)` value names.
    fn url(&self, value: &str) -> Option<Node<'a, 'i>> {
        let start = value.find("url(")? + 4;
        let inner = &value[start..];
        let inner = &inner[..inner.find(')')?];
        let id = inner.trim().trim_matches(|c| c == '"' || c == '\'').trim().strip_prefix('#')?;
        self.ids.get(id).copied()
    }
}

/// A property from the `style` attribute (which wins) or the presentation attribute.
fn prop(node: Node, name: &str) -> Option<String> {
    if let Some(style) = node.attribute("style") {
        let mut found = None;
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                if k.trim() == name {
                    found = Some(v.trim().trim_end_matches("!important").trim().to_string());
                }
            }
        }
        if found.is_some() {
            return found;
        }
    }
    node.attribute(name).map(str::to_string)
}

/// An upper bound on a shape's vertices, from how many numbers its geometry holds.
fn vertices(node: Node) -> u64 {
    let data = match node.tag_name().name() {
        "path" => node.attribute("d"),
        "polyline" | "polygon" => node.attribute("points"),
        _ => return 2,
    };
    let mut numbers = 0u64;
    let mut in_number = false;
    for c in data.unwrap_or("").chars() {
        let digit = c.is_ascii_digit();
        if digit && !in_number {
            numbers += 1;
        }
        in_number = digit || (in_number && matches!(c, '.' | 'e' | 'E'));
    }
    numbers / 2 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(r: &Rendered, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * r.width + x) * 4) as usize;
        [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
    }

    #[test]
    fn text_is_refused_but_empty_text_and_outlines_render() {
        let text = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="40"><g><text x="5" y="30"><tspan>Hi</tspan></text></g></svg>"##;
        assert_eq!(rasterize(text, 100).err().as_deref(), Some("SVG contains text"));
        let empty = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text x="1" y="1">  </text><rect width="10" height="10"/></svg>"##;
        assert!(rasterize(empty, 10).is_ok());
    }

    #[test]
    fn recognises_svg_documents_only() {
        assert!(looks_like_svg(b"<svg xmlns='http://www.w3.org/2000/svg'/>"));
        assert!(looks_like_svg(b"\xef\xbb\xbf  <?xml version='1.0'?>\n<!-- logo -->\n<!DOCTYPE svg><svg/>"));
        assert!(!looks_like_svg(b"<?xml version='1.0'?><plist/>"));
        assert!(!looks_like_svg(b"\x89PNG\r\n\x1a\n"));
        let png = rasterize_png(br##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"##, 8).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn a_wrapped_render_is_ours_and_renders_back_the_same() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="#00ff00"/></svg>"##;
        let wrapped = rasterize_wrapped(svg, 40).unwrap();
        assert!(is_raster_wrapped(&wrapped) && !is_raster_wrapped(svg));
        assert!(looks_like_svg(&wrapped), "still a valid .svg for paths that stored one");
        // Rendering the wrapper again gives the same picture: one image, nothing to expand.
        let again = rasterize(&wrapped, 40).unwrap();
        assert_eq!((again.width, again.height), (40, 20));
        assert_eq!(pixel(&again, 20, 10), [0, 255, 0, 255]);
        assert!(is_raster_wrapped(&empty_wrapped()) && rasterize(&empty_wrapped(), 4).is_ok());
    }

    #[test]
    fn renders_a_plain_svg_to_its_longest_side() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="#ff0000"/></svg>"##;
        let r = rasterize(svg, 200).unwrap();
        assert_eq!((r.width, r.height), (200, 100));
        assert_eq!(pixel(&r, 100, 50), [255, 0, 0, 255]);
    }

    #[test]
    fn scripts_and_foreign_content_never_render() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" onload="alert(1)">
            <script>alert(1)</script>
            <foreignObject width="10" height="10"><div xmlns="http://www.w3.org/1999/xhtml" style="background:red;width:10px;height:10px"></div></foreignObject>
        </svg>"##;
        let r = rasterize(svg, 10).unwrap();
        assert_eq!(pixel(&r, 5, 5)[3], 0, "nothing but pixels comes out, and these are empty");
    }

    #[test]
    fn refuses_what_a_renderer_should_never_see() {
        assert!(rasterize(&[0x1f, 0x8b, 8, 0], 64).is_err(), "SVGZ is never inflated");
        assert!(rasterize(&[0xff, 0xfe, 0x3c, 0], 64).is_err(), "not UTF-8");
        assert!(rasterize(b"<html><body/></html>", 64).is_err(), "not an <svg> root");
        assert!(rasterize(&vec![b' '; MAX_SVG_BYTES + 1], 64).is_err(), "oversized");
    }

    #[test]
    fn files_and_remote_urls_are_never_read() {
        // A real image on disk: the renderer must not pull it into the picture.
        let dir = std::env::temp_dir().join(format!("vector-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("secret.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255])).save(&png).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4">
                <image width="4" height="4" href="{p}"/><image width="4" height="4" xlink:href="file://{p}"/>
                <image width="4" height="4" href="https://example.com/x.png"/></svg>"#,
            p = png.display()
        );
        let r = rasterize(svg.as_bytes(), 4).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(pixel(&r, 2, 2)[3], 0);
    }

    #[test]
    fn embedded_images_are_raster_and_bounded() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64_simd::STANDARD.encode_to_string(&png);
        let ok = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><image width="2" height="2" href="data:image/png;base64,{b64}"/></svg>"#);
        assert_eq!(pixel(&rasterize(ok.as_bytes(), 2).unwrap(), 1, 1), [0, 0, 255, 255]);

        // A nested SVG would bring its own document past the budget: never decoded.
        let inner = base64_simd::STANDARD.encode_to_string(br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="#00ff00"/></svg>"##);
        let nested = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><image width="2" height="2" href="data:image/svg+xml;base64,{inner}"/></svg>"#);
        assert_eq!(pixel(&rasterize(nested.as_bytes(), 2).unwrap(), 1, 1)[3], 0);

        // Past the dimension cap the header alone refuses it, before anything decodes it.
        let mut wide = Vec::new();
        image::RgbaImage::from_pixel(MAX_EMBEDDED_DIM + 1, 1, image::Rgba([0, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut wide), image::ImageFormat::Png)
            .unwrap();
        assert!(embedded_image(Arc::new(wide), &AtomicUsize::new(0)).is_none());
        assert!(embedded_image(Arc::new(png), &AtomicUsize::new(0)).is_some());
    }

    #[test]
    fn entity_tricks_are_refused_or_inert() {
        let laughs = br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;"><!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;"><!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;"><!ENTITY f "&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;"><!ENTITY g "&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><text>&g;</text></svg>"#;
        assert!(rasterize(laughs, 8).is_err(), "exponential entities");
        // An external entity is never fetched: the document fails to parse rather than read the file.
        let xxe = br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/hosts">]><svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><text>&x;</text></svg>"#;
        assert!(rasterize(xxe, 8).is_err());
    }

    #[test]
    fn a_use_bomb_is_refused() {
        // Ten references per level, six levels: a million elements from a few hundred bytes.
        let mut svg = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><defs><rect id="l0" width="1" height="1"/>"#);
        for level in 1..=6 {
            svg += &format!(r#"<g id="l{level}">"#);
            for _ in 0..10 {
                svg += &format!(r##"<use xlink:href="#l{}"/>"##, level - 1);
            }
            svg += "</g>";
        }
        svg += r##"</defs><use xlink:href="#l6"/></svg>"##;
        assert_eq!(rasterize(svg.as_bytes(), 10).err().as_deref(), Some("SVG too complex to render"));
    }

    #[test]
    fn inherited_pattern_paint_is_charged_per_shape() {
        // One fill on a group, a heavy pattern, and many shapes inheriting it.
        let mut svg = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><pattern id="p" width="1" height="1">"#);
        svg += &"<rect width=\"1\" height=\"1\"/>".repeat(500);
        svg += r##"</pattern></defs><g fill="url(#p)">"##;
        svg += &"<rect width=\"1\" height=\"1\"/>".repeat(500);
        svg += "</g></svg>";
        assert_eq!(rasterize(svg.as_bytes(), 10).err().as_deref(), Some("SVG too complex to render"));
    }

    #[test]
    fn markers_are_charged_per_vertex() {
        let mut svg = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><marker id="m">"#);
        svg += &"<circle r=\"1\"/>".repeat(50);
        svg += r##"</marker></defs><path marker-mid="url(#m)" d="M0 0"##;
        svg += &" L1 1".repeat(5000);
        svg += r#""/></svg>"#;
        assert_eq!(rasterize(svg.as_bytes(), 10).err().as_deref(), Some("SVG too complex to render"));
    }

    #[test]
    fn stylesheet_references_are_charged_per_shape() {
        let mut svg = String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><style>rect { fill: url(#p) }</style><defs><pattern id="p" width="1" height="1">"##);
        svg += &"<circle r=\"1\"/>".repeat(400);
        svg += "</pattern></defs>";
        svg += &"<rect width=\"1\" height=\"1\"/>".repeat(400);
        svg += "</svg>";
        assert!(rasterize(svg.as_bytes(), 10).is_err());
    }

    #[test]
    fn a_larger_canvas_allows_fewer_layers() {
        assert_eq!(layer_limit(1024), MAX_LAYERS);
        assert_eq!(layer_limit(MAX_RENDER_DIM), 4);
        let five = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{}<rect width="1" height="1"/>{}</svg>"#,
            r#"<g opacity="0.5">"#.repeat(5),
            "</g>".repeat(5)
        );
        assert!(rasterize(five.as_bytes(), 1024).is_ok());
        assert_eq!(rasterize(five.as_bytes(), MAX_RENDER_DIM).err().as_deref(), Some("SVG stacks too many layers"));
    }

    #[test]
    fn stacked_layers_and_cycles_are_refused() {
        let deep = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{}<rect width="1" height="1"/>{}</svg>"#,
            r#"<g opacity="0.5">"#.repeat(MAX_LAYERS as usize + 1),
            "</g>".repeat(MAX_LAYERS as usize + 1)
        );
        assert_eq!(rasterize(deep.as_bytes(), 10).err().as_deref(), Some("SVG stacks too many layers"));

        let cycle = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><g id="a"><use href="#b"/></g><g id="b"><use href="#c"/></g><g id="c"><use href="#a"/></g></svg>"##;
        assert_eq!(rasterize(cycle, 10).err().as_deref(), Some("SVG references itself"));
    }

    #[test]
    fn a_duplicate_id_resolves_to_the_first_as_usvg_does() {
        // The heavy element shares an id with a cheap one that comes first: nothing references it.
        let mut svg = String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><defs><g id="x"><rect width="1" height="1"/></g><g id="x">"##);
        svg += &"<rect width=\"1\" height=\"1\"/>".repeat(2000);
        svg += "</g></defs>";
        svg += &r##"<use href="#x"/>"##.repeat(100);
        svg += "</svg>";
        assert!(rasterize(svg.as_bytes(), 10).is_ok());
    }
}
