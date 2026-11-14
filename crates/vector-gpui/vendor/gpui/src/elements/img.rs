use crate::{
    AnyElement, AnyImageCache, App, Asset, AssetLogger, Bounds, DefiniteLength, DispatchPhase,
    Element, ElementId, MotionScopes, MouseMoveEvent,
    Entity, GlobalElementId, Hitbox, Image, ImageCache, InspectorElementId, InteractiveElement,
    Interactivity, LayoutId, Length, ObjectFit, Pixels, RenderImage, Resource, SharedString,
    SharedUri, StyleRefinement, Styled, Task, Window, decode_gif_image_with, decode_static_image,
    decode_webp_image_with, px,
};
use anyhow::Result;

use futures::Future;
use gpui_util::ResultExt;
use image::{ImageError, ImageFormat};
use scheduler::Instant;
use std::{
    cell::Cell,
    fs,
    rc::Rc,
    io::{self},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

use super::{Stateful, StatefulInteractiveElement};

/// The delay before showing the loading state.
pub const LOADING_DELAY: Duration = Duration::from_millis(200);

/// A type alias to the resource loader that the `img()` element uses.
///
/// Note: that this is only for Resources, like URLs or file paths.
/// Custom loaders, or external images will not use this asset loader
pub type ImgResourceLoader = AssetLogger<ImageAssetLoader>;

/// A source of image content.
#[derive(Clone)]
pub enum ImageSource {
    /// The image content will be loaded from some resource location
    Resource(Resource),
    /// Cached image data
    Render(Arc<RenderImage>),
    /// Cached image data
    Image(Arc<Image>),
    /// A custom loading function to use
    Custom(Arc<dyn Fn(&mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>>>),
}

fn is_uri(uri: &str) -> bool {
    url::Url::from_str(uri).is_ok()
}

impl From<SharedUri> for ImageSource {
    fn from(value: SharedUri) -> Self {
        Self::Resource(Resource::Uri(value))
    }
}

impl<'a> From<&'a str> for ImageSource {
    fn from(s: &'a str) -> Self {
        if is_uri(s) {
            Self::Resource(Resource::Uri(s.to_string().into()))
        } else {
            Self::Resource(Resource::Embedded(s.to_string().into()))
        }
    }
}

impl From<String> for ImageSource {
    fn from(s: String) -> Self {
        if is_uri(&s) {
            Self::Resource(Resource::Uri(s.into()))
        } else {
            Self::Resource(Resource::Embedded(s.into()))
        }
    }
}

impl From<SharedString> for ImageSource {
    fn from(s: SharedString) -> Self {
        s.as_ref().into()
    }
}

impl From<&Path> for ImageSource {
    fn from(value: &Path) -> Self {
        Self::Resource(value.to_path_buf().into())
    }
}

impl From<Arc<Path>> for ImageSource {
    fn from(value: Arc<Path>) -> Self {
        Self::Resource(value.into())
    }
}

impl From<PathBuf> for ImageSource {
    fn from(value: PathBuf) -> Self {
        Self::Resource(value.into())
    }
}

impl From<Arc<RenderImage>> for ImageSource {
    fn from(value: Arc<RenderImage>) -> Self {
        Self::Render(value)
    }
}

impl From<Arc<Image>> for ImageSource {
    fn from(value: Arc<Image>) -> Self {
        Self::Image(value)
    }
}

impl<F> From<F> for ImageSource
where
    F: Fn(&mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>> + 'static,
{
    fn from(value: F) -> Self {
        Self::Custom(Arc::new(value))
    }
}

/// The style of an image element.
pub struct ImageStyle {
    grayscale: bool,
    object_fit: ObjectFit,
    loading: Option<Box<dyn Fn() -> AnyElement>>,
    fallback: Option<Box<dyn Fn() -> AnyElement>>,
}

impl Default for ImageStyle {
    fn default() -> Self {
        Self {
            grayscale: false,
            object_fit: ObjectFit::Contain,
            loading: None,
            fallback: None,
        }
    }
}

/// Style an image element.
pub trait StyledImage: Sized {
    /// Get a mutable [ImageStyle] from the element.
    fn image_style(&mut self) -> &mut ImageStyle;

    /// Set the image to be displayed in grayscale.
    fn grayscale(mut self, grayscale: bool) -> Self {
        self.image_style().grayscale = grayscale;
        self
    }

    /// Set the object fit for the image.
    fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.image_style().object_fit = object_fit;
        self
    }

    /// Set a fallback function that will be invoked to render an error view should
    /// the image fail to load.
    fn with_fallback(mut self, fallback: impl Fn() -> AnyElement + 'static) -> Self {
        self.image_style().fallback = Some(Box::new(fallback));
        self
    }

    /// Set a fallback function that will be invoked to render a view while the image
    /// is still being loaded.
    fn with_loading(mut self, loading: impl Fn() -> AnyElement + 'static) -> Self {
        self.image_style().loading = Some(Box::new(loading));
        self
    }
}

impl StyledImage for Img {
    fn image_style(&mut self) -> &mut ImageStyle {
        &mut self.style
    }
}

impl Stateful<Img> {
    /// See [`Img::face`].
    pub fn face(mut self) -> Self {
        self.element.face = true;
        self
    }
}

impl StyledImage for Stateful<Img> {
    fn image_style(&mut self) -> &mut ImageStyle {
        &mut self.element.style
    }
}

/// An image element.
pub struct Img {
    interactivity: Interactivity,
    source: ImageSource,
    style: ImageStyle,
    image_cache: Option<AnyImageCache>,
    face: bool,
    /// The source this frame draws: a face's first frame alone until it may move.
    shown: Option<ImageSource>,
}

/// Create a new image element.
#[track_caller]
pub fn img(source: impl Into<ImageSource>) -> Img {
    Img {
        interactivity: Interactivity::new(),
        source: source.into(),
        style: ImageStyle::default(),
        image_cache: None,
        face: false,
        shown: None,
    }
}

impl Img {
    /// Hold an animation on its first frame unless the nearest motion scope lets it move: a
    /// live one always, a host while hovered. See [`InteractiveElement::motion_host`].
    pub fn face(mut self) -> Self {
        self.face = true;
        self
    }

    /// A list of all format extensions currently supported by this img element
    pub fn extensions() -> &'static [&'static str] {
        // This is the list in [image::ImageFormat::from_extension] + `svg`
        &[
            "avif", "jpg", "jpeg", "png", "gif", "webp", "tif", "tiff", "tga", "dds", "bmp", "ico",
            "hdr", "exr", "pbm", "pam", "ppm", "pgm", "ff", "farbfeld", "qoi", "svg",
        ]
    }

    /// Sets the image cache for the current node.
    ///
    /// If the `image_cache` is not explicitly provided, the function will determine the image cache by:
    ///
    /// 1. Checking if any ancestor node of the current node contains an `ImageCacheElement`, If such a node exists, the image cache specified by that ancestor will be used.
    /// 2. If no ancestor node contains an `ImageCacheElement`, the global image cache will be used as a fallback.
    ///
    /// This mechanism provides a flexible way to manage image caching, allowing precise control when needed,
    /// while ensuring a default behavior when no cache is explicitly specified.
    #[inline]
    pub fn image_cache<I: ImageCache>(self, image_cache: &Entity<I>) -> Self {
        Self {
            image_cache: Some(image_cache.clone().into()),
            ..self
        }
    }
}

impl Deref for Stateful<Img> {
    type Target = Img;

    fn deref(&self) -> &Self::Target {
        &self.element
    }
}

impl DerefMut for Stateful<Img> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.element
    }
}

/// The image state between frames
struct ImgState {
    frame_index: usize,
    last_frame_time: Option<Instant>,
    started_loading: Option<(Instant, Task<()>)>,
    hovered: Rc<Cell<bool>>,
    clock: Rc<Cell<(usize, Option<Instant>)>>,
}

/// The image layout state between frames
pub struct ImgLayoutState {
    frame_index: usize,
    replacement: Option<AnyElement>,
    hover_gate: Option<Rc<Cell<bool>>>,
    clock: Option<Rc<Cell<(usize, Option<Instant>)>>>,
}

impl Element for Img {
    type RequestLayoutState = ImgLayoutState;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.interactivity.element_id.clone()
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        self.interactivity.source_location()
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut layout_state = ImgLayoutState {
            frame_index: 0,
            replacement: None,
            hover_gate: None,
            clock: None,
        };

        window.with_optional_element_state(global_id, |state, window| {
            let mut state = state.map(|state| {
                state.unwrap_or(ImgState {
                    frame_index: 0,
                    last_frame_time: None,
                    started_loading: None,
                    hovered: Rc::default(),
                    clock: Rc::default(),
                })
            });

            let mut frame_index = state.as_ref().map(|state| state.frame_index).unwrap_or(0);

            let layout_id = self.interactivity.request_layout(
                global_id,
                inspector_id,
                window,
                cx,
                |mut style, window, cx| {
                    let mut replacement_id = None;
                    let mut next_frame_in = None;
                    let cache = self
                        .image_cache
                        .clone()
                        .or_else(|| window.image_cache_stack.last().cloned());
                    // A held face decodes its first frame alone; once it may move, the whole
                    // strip loads behind that frame.
                    let shown = match self.face.then(|| still_of(&self.source)).flatten() {
                        Some(still) if state.as_ref().is_some_and(|s| s.hovered.get()) => {
                            match self.source.use_data(cache.clone(), window, cx) {
                                Some(Ok(_)) => self.source.clone(),
                                _ => still,
                            }
                        }
                        Some(still) => still,
                        None => self.source.clone(),
                    };
                    self.shown = Some(shown.clone());

                    match shown.use_data(cache, window, cx) {
                        Some(Ok(data)) => {
                            let frame_count = data.frame_count();
                            let max_frame_index = frame_count.saturating_sub(1);

                            if let Some(state) = &mut state {
                                (state.frame_index, state.last_frame_time) = state.clock.get();
                                state.frame_index = state.frame_index.min(max_frame_index);
                                let still = (frame_count > 1 || data.partial)
                                    && self.face
                                    && {
                                        layout_state.hover_gate = Some(state.hovered.clone());
                                        !state.hovered.get()
                                    };
                                if still {
                                    state.frame_index = 0;
                                }
                                if frame_count > 1 && !cx.reduce_motion() && !still {
                                    if window.is_window_active() && !animations_asleep() {
                                        // Every copy of a picture reads one clock, so all show the same frame.
                                        let current_time = Instant::now();
                                        let (ix, left) = shared_frame(&data, current_time.saturating_duration_since(anim_epoch()));
                                        state.frame_index = ix;
                                        state.last_frame_time = Some(current_time);
                                        next_frame_in = Some(left);
                                    } else {
                                        state.last_frame_time = None;
                                    }
                                } else {
                                    state.last_frame_time = None;
                                }
                                state.started_loading = None;
                                frame_index = state.frame_index;
                                state.clock.set((state.frame_index, state.last_frame_time));
                                if next_frame_in.is_some() {
                                    layout_state.clock = Some(state.clock.clone());
                                }
                            }

                            let image_size = data.render_size(frame_index);

                            // With both sides given, a ratio would let a min-size of auto outgrow them.
                            if style.aspect_ratio.is_none()
                                && (matches!(style.size.width, Length::Auto)
                                    || matches!(style.size.height, Length::Auto))
                            {
                                style.aspect_ratio = Some(image_size.width / image_size.height);
                            }

                            if let Length::Auto = style.size.width {
                                style.size.width = match style.size.height {
                                    Length::Definite(DefiniteLength::Absolute(abs_length)) => {
                                        let height_px = abs_length.to_pixels(window.rem_size());
                                        Length::Definite(
                                            px(image_size.width.0 * height_px.0
                                                / image_size.height.0)
                                            .into(),
                                        )
                                    }
                                    _ => Length::Definite(image_size.width.into()),
                                };
                            }

                            if let Length::Auto = style.size.height {
                                style.size.height = match style.size.width {
                                    Length::Definite(DefiniteLength::Absolute(abs_length)) => {
                                        let width_px = abs_length.to_pixels(window.rem_size());
                                        Length::Definite(
                                            px(image_size.height.0 * width_px.0
                                                / image_size.width.0)
                                            .into(),
                                        )
                                    }
                                    _ => Length::Definite(image_size.height.into()),
                                };
                            }

                            if global_id.is_some()
                                && let Some(due) = next_frame_in
                            {
                                wake_animations_in(window, cx, due);
                            }
                        }
                        Some(_err) => {
                            if self.image_cache.is_none()
                                && window.image_cache_stack.is_empty()
                                && let ImageSource::Resource(resource) = &shown
                            {
                                match ask_again(resource) {
                                    Some(Duration::ZERO) => {
                                        cx.remove_asset::<ImgResourceLoader>(resource);
                                    }
                                    Some(wait) => wake_window_in(window, cx, wait),
                                    None => {}
                                }
                            }
                            if let Some(fallback) = self.style.fallback.as_ref() {
                                let mut element = fallback();
                                replacement_id = Some(element.request_layout(window, cx));
                                layout_state.replacement = Some(element);
                            }
                            if let Some(state) = &mut state {
                                state.started_loading = None;
                            }
                        }
                        None => {
                            if let Some(state) = &mut state {
                                if let Some((started_loading, _)) = state.started_loading {
                                    if started_loading.elapsed() > LOADING_DELAY
                                        && let Some(loading) = self.style.loading.as_ref()
                                    {
                                        let mut element = loading();
                                        replacement_id = Some(element.request_layout(window, cx));
                                        layout_state.replacement = Some(element);
                                    }
                                } else {
                                    let current_view = window.current_view();
                                    let task = window.spawn(cx, async move |cx| {
                                        cx.background_executor().timer(LOADING_DELAY).await;
                                        cx.update(move |_, cx| {
                                            cx.notify(current_view);
                                        })
                                        .ok();
                                    });
                                    state.started_loading = Some((Instant::now(), task));
                                }
                            }
                        }
                    }

                    window.request_layout(style, replacement_id, cx)
                },
            );

            layout_state.frame_index = frame_index;

            ((layout_id, layout_state), state)
        })
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.interactivity.prepaint(
            global_id,
            inspector_id,
            bounds,
            bounds.size,
            window,
            cx,
            |_, _, hitbox, window, cx| {
                if let Some(replacement) = &mut request_layout.replacement {
                    replacement.prepaint(window, cx);
                }

                hitbox
            },
        )
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let source = self.shown.clone().unwrap_or_else(|| self.source.clone());
        if let Some(hovered) = layout_state.hover_gate.take() {
            // Only this picture's view redraws: cached views elsewhere keep their frame.
            let view = window.current_view();
            let host = MotionScopes::current(cx);
            let moving = match host {
                Some(Some(hitbox)) => hitbox.is_hovered(window),
                Some(None) => true,
                None => false,
            };
            if moving != hovered.get() {
                hovered.set(moving);
                cx.notify(view);
            }
            if let Some(Some(hitbox)) = host {
                window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && hitbox.is_hovered(window) != hovered.get()
                    {
                        hovered.set(!hovered.get());
                        cx.notify(view);
                    }
                });
            }
        }
        self.interactivity.paint(
            global_id,
            inspector_id,
            bounds,
            hitbox.as_ref(),
            window,
            cx,
            |style, window, cx| {
                if let Some(Ok(data)) = source.use_data(
                    self.image_cache
                        .clone()
                        .or_else(|| window.image_cache_stack.last().cloned()),
                    window,
                    cx,
                ) {
                    if data.frame_count() == 0 {
                        return;
                    }
                    let new_bounds = self
                        .style
                        .object_fit
                        .get_bounds(bounds, data.size(layout_state.frame_index));
                    let corner_radii = style.corner_radii.to_pixels(window.rem_size());
                    match layout_state.clock.take() {
                        Some(clock) => window.paint_animated_image(
                            bounds,
                            new_bounds,
                            corner_radii,
                            data,
                            clock,
                            self.style.grayscale,
                        ),
                        None => window.paint_image(
                            bounds,
                            new_bounds,
                            corner_radii,
                            data,
                            layout_state.frame_index,
                            self.style.grayscale,
                        ),
                    }
                    .log_err();
                } else if let Some(replacement) = &mut layout_state.replacement {
                    replacement.paint(window, cx);
                }
            },
        )
    }
}

impl Styled for Img {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.interactivity.base_style
    }
}

impl InteractiveElement for Img {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl StatefulInteractiveElement for Img {}

impl ImageSource {
    pub(crate) fn use_data(
        &self,
        cache: Option<AnyImageCache>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        match self {
            ImageSource::Resource(resource) => {
                if let Some(cache) = cache {
                    cache.load(resource, window, cx)
                } else {
                    window.use_asset::<ImgResourceLoader>(resource, cx)
                }
            }
            ImageSource::Custom(loading_fn) => loading_fn(window, cx),
            ImageSource::Render(data) => Some(Ok(data.to_owned())),
            ImageSource::Image(data) => window.use_asset::<AssetLogger<ImageDecoder>>(data, cx),
        }
    }

    pub(crate) fn get_data(
        &self,
        cache: Option<AnyImageCache>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        match self {
            ImageSource::Resource(resource) => {
                if let Some(cache) = cache {
                    cache.load(resource, window, cx)
                } else {
                    window.get_asset::<ImgResourceLoader>(resource, cx)
                }
            }
            ImageSource::Custom(loading_fn) => loading_fn(window, cx),
            ImageSource::Render(data) => Some(Ok(data.to_owned())),
            ImageSource::Image(data) => window.get_asset::<AssetLogger<ImageDecoder>>(data, cx),
        }
    }

    /// Remove this image source from the asset system
    pub fn remove_asset(&self, cx: &mut App) {
        match self {
            ImageSource::Resource(resource) => {
                cx.remove_asset::<ImgResourceLoader>(resource);
            }
            ImageSource::Custom(_) | ImageSource::Render(_) => {}
            ImageSource::Image(data) => cx.remove_asset::<AssetLogger<ImageDecoder>>(data),
        }
    }

    /// Check whether this image source is present in the asset system (loading
    /// or loaded), without fetching it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_asset_cached(&self, cx: &App) -> bool {
        match self {
            ImageSource::Resource(resource) => cx.has_asset::<ImgResourceLoader>(resource),
            ImageSource::Custom(_) | ImageSource::Render(_) => false,
            ImageSource::Image(data) => cx.has_asset::<AssetLogger<ImageDecoder>>(data),
        }
    }
}

#[derive(Clone)]
#[doc(hidden)]
pub enum ImageDecoder {}

impl Asset for ImageDecoder {
    type Source = Arc<Image>;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        async move { source.to_image_data(renderer).map_err(Into::into) }
    }
}

/// An image loader for the GPUI asset system
#[derive(Clone)]
pub enum ImageAssetLoader {}

impl Asset for ImageAssetLoader {
    type Source = Resource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let client = cx.http_client();
        // TODO: Can we make SVGs always rescale?
        // let scale_factor = cx.scale_factor();
        let svg_renderer = cx.svg_renderer();
        let asset_source = cx.asset_source().clone();
        async move {
            let mut fit = None;
            let mut still = false;
            let bytes = match source.clone() {
                Resource::Path(uri) => fs::read(uri.as_ref())?,
                Resource::Uri(uri) => {
                    use anyhow::Context as _;
                    use futures::AsyncReadExt as _;

                    let mut response = client
                        .get(uri.as_ref(), ().into(), true)
                        .await
                        .with_context(|| format!("loading image asset from {uri:?}"))?;
                    let mut body = Vec::new();
                    response.body_mut().read_to_end(&mut body).await?;
                    if !response.status().is_success() {
                        let mut body = String::from_utf8_lossy(&body).into_owned();
                        let first_line = body.lines().next().unwrap_or("").trim_end();
                        body.truncate(first_line.len());
                        return Err(ImageCacheError::BadStatus {
                            uri,
                            status: response.status(),
                            body,
                        });
                    }
                    body
                }
                Resource::Embedded(path) => {
                    let path = match path.strip_suffix(STILL) {
                        Some(name) => {
                            still = true;
                            SharedString::from(name.to_string())
                        }
                        None => path,
                    };
                    let path = match path.rsplit_once(FIT) {
                        Some((name, n)) if n.parse::<u32>().is_ok() => {
                            fit = n.parse().ok().map(|n| (n, false));
                            SharedString::from(name.to_string())
                        }
                        _ => match path.rsplit_once(SQUARE) {
                            Some((name, n)) if n.parse::<u32>().is_ok() => {
                                fit = n.parse().ok().map(|n| (n, true));
                                SharedString::from(name.to_string())
                            }
                            _ => path,
                        },
                    };
                    let data = asset_source.load(&path).ok().flatten();
                    if let Some(data) = data {
                        data.to_vec()
                    } else {
                        return Err(ImageCacheError::Asset(
                            format!("Embedded resource not found: {}", path).into(),
                        ));
                    }
                }
            };

            if let Ok(format) = image::guess_format(&bytes) {
                let each = |frame: image::Frame| match fit {
                    Some((max, square)) => fit_frame(frame, max, square),
                    None => frame,
                };
                let limit = still.then_some(1);
                let (data, partial) = match format {
                    ImageFormat::Gif => decode_gif_image_with(&bytes, each, limit)?,
                    ImageFormat::WebP => decode_webp_image_with(&bytes, each, limit)?,
                    _ => (decode_static_image(&bytes, format)?.into_iter().map(each).collect(), false),
                };
                let mut image = RenderImage::new(data);
                image.partial = partial;
                Ok(Arc::new(image))
            } else {
                svg_renderer
                    .render_single_frame(&bytes, 1.0)
                    .map_err(Into::into)
            }
        }
    }
}

/// Suffix on an embedded asset's name, `#fit=N`: decoded pictures are shrunk to fit N pixels.
pub const FIT: &str = "#fit=";

/// Like [`FIT`], but first cut to the centred square: for a picture always drawn square or round.
pub const SQUARE: &str = "#square=";

/// How long a picture that failed to load waits before asking for its file again.
const RETRY_AFTER: Duration = Duration::from_secs(1);

/// A file that failed to load is asked for again twice a session, a while after each failure,
/// then left as it is: one written a moment late or let go of recovers, one truly gone doesn't
/// loop. `Some(ZERO)` means ask now, `Some(wait)` to look again after it.
fn ask_again(resource: &Resource) -> Option<Duration> {
    thread_local!(static ASKED: std::cell::RefCell<collections::FxHashMap<u64, (u8, Instant)>> = Default::default());
    let key = crate::hash(resource);
    ASKED.with(|a| {
        let mut a = a.borrow_mut();
        let (tries, since) = a.entry(key).or_insert((0, Instant::now()));
        if *tries >= 2 {
            return None;
        }
        let waited = since.elapsed();
        if waited < RETRY_AFTER {
            return Some(RETRY_AFTER - waited);
        }
        *tries += 1;
        *since = Instant::now();
        Some(Duration::ZERO)
    })
}

/// Suffix on an embedded asset's name, after any fit: decode the first frame alone, noting
/// whether the source has more ([`RenderImage`]'s `partial`).
pub const STILL: &str = "#still";

/// The first-frame form of `source`, where its name can carry the hint.
fn still_of(source: &ImageSource) -> Option<ImageSource> {
    match source {
        ImageSource::Resource(Resource::Embedded(name)) => {
            Some(ImageSource::Resource(Resource::Embedded(format!("{name}{STILL}").into())))
        }
        _ => None,
    }
}

/// Shrinks a frame whose longer side passes `max`; a picture shown small never holds its full size.
fn fit_frame(frame: image::Frame, max: u32, square: bool) -> image::Frame {
    let (w, h) = frame.buffer().dimensions();
    let (cw, ch) = if square { (w.min(h), w.min(h)) } else { (w, h) };
    if (cw == w && ch == h && w.max(h) <= max) || max == 0 || cw == 0 {
        return frame;
    }
    let scale = (max as f64 / cw.max(ch) as f64).min(1.);
    let (nw, nh) = (
        ((cw as f64 * scale).round() as u32).max(1),
        ((ch as f64 * scale).round() as u32).max(1),
    );
    let (cx, cy) = ((w - cw) / 2, (h - ch) / 2);
    let delay = frame.delay();
    let (left, top) = (frame.left(), frame.top());
    let cut = image::imageops::crop_imm(frame.buffer(), cx, cy, cw, ch).to_image();
    let buffer = if (nw, nh) == (cw, ch) {
        cut
    } else {
        image::imageops::resize(&cut, nw, nh, image::imageops::FilterType::Triangle)
    };
    image::Frame::from_parts(
        buffer,
        (left.saturating_sub(cx) as f64 * scale) as u32,
        (top.saturating_sub(cy) as f64 * scale) as u32,
        delay,
    )
}

/// An error that can occur when interacting with the image cache.
#[derive(Debug, Error, Clone)]
pub enum ImageCacheError {
    /// Some other kind of error occurred
    #[error("error: {0}")]
    Other(#[from] Arc<anyhow::Error>),
    /// An error that occurred while reading the image from disk.
    #[error("IO error: {0}")]
    Io(Arc<std::io::Error>),
    /// An error that occurred while processing an image.
    #[error("unexpected http status for {uri}: {status}, body: {body}")]
    BadStatus {
        /// The URI of the image.
        uri: SharedUri,
        /// The HTTP status code.
        status: http_client::StatusCode,
        /// The HTTP response body.
        body: String,
    },
    /// An error that occurred while processing an asset.
    #[error("asset error: {0}")]
    Asset(SharedString),
    /// An error that occurred while processing an image.
    #[error("image error: {0}")]
    Image(Arc<ImageError>),
    /// An error that occurred while processing an SVG.
    #[error("svg error: {0}")]
    Usvg(Arc<usvg::Error>),
}

impl From<anyhow::Error> for ImageCacheError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(Arc::new(value))
    }
}

impl From<io::Error> for ImageCacheError {
    fn from(value: io::Error) -> Self {
        Self::Io(Arc::new(value))
    }
}

impl From<usvg::Error> for ImageCacheError {
    fn from(value: usvg::Error) -> Self {
        Self::Usvg(Arc::new(value))
    }
}

impl From<image::ImageError> for ImageCacheError {
    fn from(value: image::ImageError) -> Self {
        Self::Image(Arc::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IntoElement, ParentElement as _, TestAppContext, canvas, div, point, px, size};
    use image::{Frame, ImageBuffer, Rgba};
    use smallvec::SmallVec;

    const TEST_IMG_ID: &str = "test-img";

    fn test_image(frame_count: usize) -> Arc<RenderImage> {
        let frame = Frame::new(ImageBuffer::from_pixel(1, 1, Rgba([0, 0, 0, 0])));
        Arc::new(RenderImage::new(SmallVec::from_iter(
            (0..frame_count).map(|_| frame.clone()),
        )))
    }

    fn test_image_with_size(width: u32, height: u32) -> Arc<RenderImage> {
        let frame = Frame::new(ImageBuffer::from_pixel(width, height, Rgba([0, 0, 0, 0])));
        Arc::new(RenderImage::new(SmallVec::from_elem(frame, 1)))
    }

    /// Overwrites the cached `frame_index` of the sibling `img` during paint.
    fn seed_frame_index(frame_index: usize) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.with_global_id(TEST_IMG_ID.into(), |id, window| {
                    window.with_element_state::<ImgState, _>(id, |state, _| {
                        let mut state = state.expect("img state should be initialized");
                        state.frame_index = frame_index;
                        ((), state)
                    });
                });
            },
        )
    }

    #[gpui::test]
    fn zero_frame_image_does_not_panic_on_paint(cx: &mut TestAppContext) {
        cx.add_empty_window()
            .draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
                img(ImageSource::Render(test_image(0))).into_any_element()
            });
    }

    #[gpui::test]
    fn image_object_fit_cover_crops_to_element_bounds(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        let image = test_image_with_size(200, 100);
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(image.clone()))
                .size_full()
                .object_fit(ObjectFit::Fill)
                .into_any_element()
        });
        let full_tile_bounds = window.update(|window, _| {
            window
                .rendered_frame
                .scene
                .polychrome_sprites
                .last()
                .expect("fill image should paint a sprite")
                .tile
                .bounds
        });

        window.draw(point(px(10.), px(20.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(image))
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        });

        let (rendered_bounds, rendered_tile_bounds, scale_factor) = window.update(|window, _| {
            let sprite = window
                .rendered_frame
                .scene
                .polychrome_sprites
                .last()
                .expect("cover image should paint a sprite");
            (sprite.bounds, sprite.tile.bounds, window.scale_factor())
        });
        assert_eq!(
            rendered_bounds,
            Bounds {
                origin: point(px(10.).scale(scale_factor), px(20.).scale(scale_factor)),
                size: size(px(100.).scale(scale_factor), px(100.).scale(scale_factor)),
            }
        );
        assert_eq!(
            (
                rendered_tile_bounds.origin.x.0 - full_tile_bounds.origin.x.0,
                rendered_tile_bounds.origin.y.0 - full_tile_bounds.origin.y.0,
                rendered_tile_bounds.size.width.0,
                rendered_tile_bounds.size.height.0,
            ),
            (50, 0, 100, 100),
        );
    }

    #[gpui::test]
    fn explicit_aspect_ratio_is_not_overridden_by_intrinsic_ratio(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();

        // A portrait image in a square container
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            div()
                .size(px(100.))
                .overflow_hidden()
                .child(
                    img(ImageSource::Render(test_image_with_size(100, 200)))
                        .size_full()
                        .aspect_square()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element()
        });

        let (rendered_bounds, scale_factor) = window.update(|window, _| {
            let sprite = window
                .rendered_frame
                .scene
                .polychrome_sprites
                .last()
                .expect("contained image should paint a sprite");
            (sprite.bounds, window.scale_factor())
        });

        // The element stays 100x100, so the image is letterboxed horizontally
        assert_eq!(
            rendered_bounds,
            Bounds {
                origin: point(px(25.).scale(scale_factor), px(0.).scale(scale_factor)),
                size: size(px(50.).scale(scale_factor), px(100.).scale(scale_factor)),
            }
        );
    }

    #[gpui::test]
    fn image_object_fit_cover_clamps_corner_radii_to_visible_bounds(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(test_image_with_size(200, 100)))
                .size_full()
                .rounded(px(100.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        });

        let (corner_radius, expected_corner_radius) = window.update(|window, _| {
            (
                window
                    .rendered_frame
                    .scene
                    .polychrome_sprites
                    .last()
                    .map(|sprite| sprite.corner_radii.top_left),
                px(50.).scale(window.scale_factor()),
            )
        });
        assert_eq!(corner_radius, Some(expected_corner_radius));
    }

    #[gpui::test]
    fn stale_frame_index_is_clamped_when_image_changes(cx: &mut TestAppContext) {
        let window = cx.add_empty_window();

        // Assert that a cached frame_index from a previous multi-frame image
        // does not cause an out-of-bounds panic when the image is replaced
        // with one that has fewer frames.
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            div()
                .child(img(ImageSource::Render(test_image(5))).id(TEST_IMG_ID))
                .child(seed_frame_index(4))
                .into_any_element()
        });
        window.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, _| {
            img(ImageSource::Render(test_image(1)))
                .id(TEST_IMG_ID)
                .into_any_element()
        });
    }
}

/// A frame's delay as browsers play it: 10ms or less means 100ms, so a GIF saved with no
/// delay doesn't spin at the display's refresh rate.
pub(crate) fn frame_delay(data: &RenderImage, index: usize) -> Duration {
    let delay = Duration::from(data.delay(index));
    if delay <= Duration::from_millis(10) {
        Duration::from_millis(100)
    } else {
        delay
    }
}

thread_local! {
    /// Each window's earliest pending wake of each sort, keyed by whether it redraws.
    /// Kept apart so a slow redraw never promotes the frequent animation steps into redraws.
    static WAKES: std::cell::RefCell<collections::FxHashMap<(crate::WindowId, bool), Instant>> =
        Default::default();
    /// The views each window's pending redraw serves: only they re-render, so cached views
    /// elsewhere keep their frame.
    static WAKE_VIEWS: std::cell::RefCell<collections::FxHashMap<crate::WindowId, collections::FxHashMap<crate::EntityId, Instant>>> =
        Default::default();
}

/// Redraw `window` once `due` has passed, instead of on every vsync until then.
pub fn wake_window_in(window: &mut Window, cx: &mut App, due: Duration) {
    wake(window, cx, due, true);
}

thread_local! {
    static LAST_INPUT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    static IDLE_AFTER: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::from_secs(300)) };
}

/// Someone is at the window (a mouse move, key, scroll or focus): animations run.
pub fn note_input() {
    LAST_INPUT.with(|t| t.set(Some(Instant::now())));
}

/// How long without input before animated pictures hold their frame.
pub fn set_animation_idle(after: Duration) {
    IDLE_AFTER.with(|i| i.set(after));
}

/// Nobody has touched the window for a while: animated pictures hold the frame they're on, and
/// nothing is presented for them, which lets the GPU driver release its working memory.
pub(crate) fn animations_asleep() -> bool {
    let last = LAST_INPUT.with(|t| t.get()).unwrap_or_else(|| {
        let now = Instant::now();
        LAST_INPUT.with(|t| t.set(Some(now)));
        now
    });
    last.elapsed() >= IDLE_AFTER.with(|i| i.get())
}

/// The instant every animation's clock and wake grid count from.
pub(crate) fn anim_epoch() -> Instant {
    thread_local!(static EPOCH: Instant = Instant::now());
    EPOCH.with(|e| *e)
}

/// The frame a picture shows `at` into the shared clock, and how long until the next one.
pub(crate) fn shared_frame(data: &RenderImage, at: Duration) -> (usize, Duration) {
    let count = data.frame_count();
    let total: u128 = (0..count).map(|i| frame_delay(data, i).as_nanos()).sum();
    if count < 2 || total == 0 {
        return (0, Duration::ZERO);
    }
    let mut t = at.as_nanos() % total;
    for i in 0..count {
        let d = frame_delay(data, i).as_nanos();
        if t < d {
            return (i, Duration::from_nanos((d - t) as u64));
        }
        t -= d;
    }
    (count - 1, Duration::ZERO)
}

/// Step the window's animated pictures once `due` has passed, redrawing nothing else.
pub(crate) fn wake_animations_in(window: &mut Window, cx: &mut App, due: Duration) {
    wake(window, cx, due, false);
}

fn wake(window: &mut Window, cx: &mut App, due: Duration, redraw: bool) {
    // Wakes land on one shared grid, so many animations out of phase cost one wake per
    // tick rather than one each; animation-only steps share a coarser 20 fps grid.
    let tick = Duration::from_millis(if redraw { 33 } else { 50 });
    let epoch = anim_epoch();
    let want = Instant::now() + due;
    let ticks = (want.saturating_duration_since(epoch).as_nanos() / tick.as_nanos() + 1) as u32;
    let at = epoch + tick * ticks;
    let key = (window.window_handle().window_id(), redraw);
    if redraw {
        let view = window.current_view();
        WAKE_VIEWS.with(|v| {
            let mut v = v.borrow_mut();
            let due = v.entry(key.0).or_default().entry(view).or_insert(at);
            *due = (*due).min(at);
        });
    }
    arm(window, cx, key, at);
}

fn arm(window: &mut Window, cx: &mut App, key: (crate::WindowId, bool), at: Instant) {
    let sooner = WAKES.with(|w| {
        let mut w = w.borrow_mut();
        match w.get(&key) {
            Some(pending) if *pending > Instant::now() && *pending <= at => false,
            _ => {
                w.insert(key, at);
                true
            }
        }
    });
    if !sooner {
        return;
    }
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(at - Instant::now()).await;
            let current = WAKES.with(|w| w.borrow().get(&key).copied());
            if current == Some(at) {
                WAKES.with(|w| w.borrow_mut().remove(&key));
                cx.update(|window, cx| {
                    if !key.1 {
                        return window.step_animations(cx);
                    }
                    // Only the views due now: one asking for a later tick keeps its frame.
                    let (due, next) = WAKE_VIEWS.with(|v| {
                        let mut v = v.borrow_mut();
                        let Some(views) = v.get_mut(&key.0) else {
                            return (Vec::new(), None);
                        };
                        let due: Vec<_> = views.iter().filter(|(_, t)| **t <= at).map(|(id, _)| *id).collect();
                        for id in &due {
                            views.remove(id);
                        }
                        (due, views.values().min().copied())
                    });
                    for view in due {
                        cx.notify(view);
                    }
                    if let Some(next) = next {
                        arm(window, cx, key, next);
                    }
                })
                .ok();
            }
        })
        .detach();
}
