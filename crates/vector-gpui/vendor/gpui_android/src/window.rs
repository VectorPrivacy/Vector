//! The app's one window: the activity's `SurfaceView`, drawn by wgpu. The surface comes and
//! goes with the activity; the renderer, atlas and window outlive it, so a return from the
//! background redraws without re-uploading anything.

use crate::bridge;
use crate::display::AndroidDisplay;
use crate::keys;
use crate::platform::PlatformState;
use crate::text_input::{self, Field};
use crate::CallbackSlot;
use gpui::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, Edges, GpuSpecs,
    KeyDownEvent, KeyUpEvent, Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions,
    Scene, Size, TextInputConfiguration, TextInputStateChange, TouchEvent, TouchId, TouchPhase,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowInsets,
    WindowVisibility, point, px, size,
};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};
use ndk::native_window::NativeWindow;
use raw_window_handle as rwh;
use std::cell::{Cell, RefCell};
use std::ffi::{c_long, c_void};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The surface as wgpu takes it.
#[derive(Clone, Debug)]
pub(crate) struct RawWindow(pub NativeWindow);

impl rwh::HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        rwh::HasWindowHandle::window_handle(&self.0)
    }
}

impl rwh::HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        Ok(rwh::DisplayHandle::android())
    }
}

fn surface_config(width: i32, height: i32) -> WgpuSurfaceConfig {
    WgpuSurfaceConfig {
        size: size(DevicePixels(width.max(1)), DevicePixels(height.max(1))),
        transparent: false,
        // FIFO would block in `get_current_texture` while the surface is being torn down.
        preferred_present_mode: Some(gpui_wgpu::wgpu::PresentMode::Mailbox),
    }
}

pub(crate) struct AndroidWindow(pub(crate) Rc<WindowState>);

#[derive(Default)]
struct Callbacks {
    request_frame: CallbackSlot<Box<dyn FnMut(RequestFrameOptions)>>,
    input: CallbackSlot<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    active: CallbackSlot<Box<dyn FnMut(bool)>>,
    visibility: CallbackSlot<Box<dyn FnMut(WindowVisibility)>>,
    hover: CallbackSlot<Box<dyn FnMut(bool)>>,
    resize: CallbackSlot<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: CallbackSlot<Box<dyn FnMut()>>,
    display_changed: CallbackSlot<Box<dyn FnMut()>>,
    should_close: CallbackSlot<Box<dyn FnMut() -> bool>>,
    hit_test: CallbackSlot<Box<dyn FnMut() -> Option<WindowControlArea>>>,
    close: CallbackSlot<Box<dyn FnOnce()>>,
    appearance: CallbackSlot<Box<dyn FnMut()>>,
    insets: CallbackSlot<Box<dyn FnMut(WindowInsets)>>,
    back: CallbackSlot<Box<dyn FnMut()>>,
}

pub(crate) struct WindowState {
    platform: Weak<PlatformState>,
    renderer: RefCell<WgpuRenderer>,
    gpu: GpuContext,
    surface: RefCell<Option<RawWindow>>,
    /// Device pixels.
    surface_size: Cell<(i32, i32)>,
    scale: Cell<f32>,
    dark: Cell<bool>,
    /// Device pixels: top, right, bottom, left, then the keyboard.
    raw_insets: Cell<([f32; 4], f32)>,
    visibility: Cell<WindowVisibility>,
    active: Cell<bool>,
    mouse_position: Cell<Point<Pixels>>,
    modifiers: Cell<Modifiers>,
    capslock: Cell<bool>,
    frame_pending: Cell<bool>,
    force_next_frame: Cell<bool>,
    /// The last draw's time, its acquire, submit and present, and its scene's quads, shadows,
    /// paths, glyphs and images.
    drawn: Cell<Option<(Duration, [Duration; 3], [usize; 5])>>,
    /// When a slow frame was last logged, and how many have gone unlogged since.
    slow: Cell<(Option<Instant>, u32)>,
    input_handler: CallbackSlot<PlatformInputHandler>,
    /// The field as the keyboard's copy last stood, when both sides agreed.
    field: RefCell<Option<Field>>,
    ime_serial: Cell<i32>,
    callbacks: Callbacks,
}

thread_local! {
    static FRAME_WINDOW: RefCell<Weak<WindowState>> = const { RefCell::new(Weak::new()) };
}

unsafe extern "C" fn on_frame(_frame_time_nanos: c_long, _data: *mut c_void) {
    // A panic cannot unwind into the Choreographer: it would abort the process.
    let drawn = std::panic::catch_unwind(|| {
        if let Some(window) = FRAME_WINDOW.with(|w| w.borrow().upgrade()) {
            window.frame_pending.set(false);
            window.request_frame();
        }
    });
    if drawn.is_err() {
        log::error!("gpui_android: a frame panicked; skipping it");
    }
}

impl AndroidWindow {
    pub fn new(
        _handle: AnyWindowHandle,
        platform: &Rc<PlatformState>,
        surface: NativeWindow,
    ) -> anyhow::Result<Self> {
        let config = platform.config.get();
        let (width, height) = (surface.width(), surface.height());
        let raw = RawWindow(surface);
        let gpu = platform.gpu.clone();
        let renderer = WgpuRenderer::new(gpu.clone(), &raw, surface_config(width, height), None)?;
        let state = Rc::new(WindowState {
            platform: Rc::downgrade(platform),
            renderer: RefCell::new(renderer),
            gpu,
            surface: RefCell::new(Some(raw)),
            surface_size: Cell::new((width, height)),
            scale: Cell::new(config.density),
            dark: Cell::new(config.dark),
            raw_insets: Cell::new(platform.insets.get()),
            visibility: Cell::new(WindowVisibility::Visible),
            active: Cell::new(true),
            mouse_position: Cell::new(Point::default()),
            modifiers: Cell::new(Modifiers::default()),
            capslock: Cell::new(false),
            frame_pending: Cell::new(false),
            force_next_frame: Cell::new(true),
            drawn: Cell::new(None),
            slow: Cell::new((None, 0)),
            input_handler: CallbackSlot::default(),
            field: RefCell::new(None),
            ime_serial: Cell::new(0),
            callbacks: Callbacks::default(),
        });
        FRAME_WINDOW.with(|w| *w.borrow_mut() = Rc::downgrade(&state));
        Ok(Self(state))
    }
}

impl WindowState {
    fn content_size(&self) -> Size<Pixels> {
        let (w, h) = self.surface_size.get();
        let scale = self.scale.get();
        size(px(w as f32 / scale), px(h as f32 / scale))
    }

    pub(crate) fn display(&self) -> AndroidDisplay {
        AndroidDisplay {
            size: self.content_size(),
            refresh_hz: self.platform.upgrade().map_or(60., |p| p.config.get().refresh_hz),
        }
    }

    /// Asks the choreographer for the next vsync, once.
    pub(crate) fn schedule_frame(&self) {
        if self.frame_pending.replace(true) {
            return;
        }
        // SAFETY: GPUI's thread has a looper, which the choreographer needs; the callback
        // takes no data.
        unsafe {
            let choreographer = ndk_sys::AChoreographer_getInstance();
            if choreographer.is_null() {
                self.frame_pending.set(false);
                return;
            }
            #[allow(deprecated)]
            ndk_sys::AChoreographer_postFrameCallback(choreographer, Some(on_frame), std::ptr::null_mut());
        }
    }

    fn request_frame(&self) {
        if self.surface.borrow().is_none() || self.visibility.get() == WindowVisibility::Hidden {
            return;
        }
        let force_render = self.force_next_frame.replace(false);
        let started = Instant::now();
        self.callbacks.request_frame.with(|callback| {
            callback(RequestFrameOptions {
                force_render,
                ..Default::default()
            })
        });
        self.note_frame(started.elapsed());
    }

    /// A slow frame, at most once a second: how long it took, how much of that was the renderer,
    /// and what it drew.
    fn note_frame(&self, took: Duration) {
        const SLOW: Duration = Duration::from_millis(100);
        let Some((drawing, [acquire, submit, present], [quads, shadows, paths, glyphs, images])) = self.drawn.take() else { return };
        if took < SLOW {
            return;
        }
        let (last, unlogged) = self.slow.get();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            self.slow.set((last, unlogged + 1));
            return;
        }
        self.slow.set((Some(Instant::now()), 0));
        log::info!(
            "gpui_android: slow frame {} ms, {} ms of it rendering (acquire {}, record and submit {}, present {}; {quads} quads, {shadows} shadows, {paths} paths, {glyphs} glyphs, {images} images); {unlogged} more slow since the last line",
            took.as_millis(),
            drawing.as_millis(),
            acquire.as_millis(),
            submit.as_millis(),
            present.as_millis()
        );
    }

    fn redraw_all(&self) {
        self.force_next_frame.set(true);
        self.schedule_frame();
    }

    fn dispatch(&self, input: PlatformInput) -> DispatchEventResult {
        self.callbacks
            .input
            .with(|callback| callback(input))
            .unwrap_or_default()
    }

    pub(crate) fn attach_surface(&self, surface: NativeWindow) {
        let (width, height) = (surface.width(), surface.height());
        let raw = RawWindow(surface);
        let instance = self.gpu.borrow().as_ref().map(|context| context.instance.clone());
        let Some(instance) = instance else {
            return;
        };
        if let Err(error) =
            self.renderer
                .borrow_mut()
                .replace_surface(&raw, surface_config(width, height), &instance)
        {
            log::error!("gpui_android: the new surface could not be used: {error:#}");
            return;
        }
        self.surface.replace(Some(raw));
        self.set_surface_size(width, height);
        self.redraw_all();
    }

    pub(crate) fn detach_surface(&self) {
        self.renderer.borrow_mut().unconfigure_surface();
        self.surface.replace(None);
    }

    pub(crate) fn set_surface_size(&self, width: i32, height: i32) {
        if self.surface_size.replace((width, height)) == (width, height) {
            return;
        }
        self.renderer
            .borrow_mut()
            .update_drawable_size(size(DevicePixels(width), DevicePixels(height)));
        let content = self.content_size();
        let scale = self.scale.get();
        self.callbacks.resize.with(|callback| callback(content, scale));
        self.redraw_all();
    }

    pub(crate) fn set_configuration(&self, density: f32, dark: bool) {
        if self.scale.replace(density) != density {
            let content = self.content_size();
            self.callbacks.resize.with(|callback| callback(content, density));
            self.notify_insets();
        }
        if self.dark.replace(dark) != dark {
            self.callbacks.appearance.with(|callback| callback());
        }
        self.redraw_all();
    }

    pub(crate) fn set_insets(&self, safe: [f32; 4], ime: f32) {
        if self.raw_insets.replace((safe, ime)) != (safe, ime) {
            self.notify_insets();
        }
    }

    fn current_insets(&self) -> WindowInsets {
        let ([top, right, bottom, left], ime) = self.raw_insets.get();
        let scale = self.scale.get();
        WindowInsets {
            safe_area: Edges {
                top: px(top / scale),
                right: px(right / scale),
                bottom: px(bottom / scale),
                left: px(left / scale),
            },
            ime: Edges {
                bottom: px(ime / scale),
                ..Default::default()
            },
            corner_radii: Default::default(),
        }
    }

    fn notify_insets(&self) {
        let insets = self.current_insets();
        self.callbacks.insets.with(|callback| callback(insets));
        self.schedule_frame();
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        let visibility = if visible { WindowVisibility::Visible } else { WindowVisibility::Hidden };
        if self.visibility.replace(visibility) != visibility {
            self.callbacks.visibility.with(|callback| callback(visibility));
            if visible {
                self.redraw_all();
            }
        }
    }

    pub(crate) fn set_active(&self, active: bool) {
        if self.active.replace(active) != active {
            self.callbacks.active.with(|callback| callback(active));
        }
    }

    pub(crate) fn handle_touch(&self, id: i32, phase: i32, x: f32, y: f32, pressure: f32, time: Instant) {
        let phase = match phase {
            0 => TouchPhase::Started,
            1 => TouchPhase::Moved,
            2 => TouchPhase::Ended,
            _ => TouchPhase::Cancelled,
        };
        let scale = self.scale.get();
        let position = point(px(x / scale), px(y / scale));
        self.mouse_position.set(position);
        self.dispatch(PlatformInput::Touch(TouchEvent {
            id: TouchId(id as u64),
            phase,
            position,
            predicted_position: None,
            force: (pressure > 0.).then(|| pressure.clamp(0., 1.)),
            time: Some(time),
        }));
        if phase == TouchPhase::Ended {
            self.sync_field();
        }
    }

    pub(crate) fn handle_key(&self, down: bool, key_code: i32, unicode: u32, base: u32, meta: i32, repeat: i32) {
        self.modifiers.set(keys::modifiers(meta));
        self.capslock.set(meta & keys::META_CAPS_LOCK_ON != 0);
        let Some(keystroke) = keys::keystroke(key_code, unicode, base, meta) else {
            return;
        };
        if !down {
            self.dispatch(PlatformInput::KeyUp(KeyUpEvent { keystroke }));
            return;
        }
        self.press(keystroke, repeat > 0);
    }

    fn press(&self, keystroke: gpui::Keystroke, is_held: bool) {
        let typed = keystroke
            .modifiers
            .is_subset_of(&Modifiers::shift())
            .then(|| keystroke.key_char.clone())
            .flatten();
        let result = self.dispatch(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held,
            prefer_character_input: false,
        }));
        if result.propagate
            && let Some(text) = typed
        {
            self.input_handler.with(|handler| handler.replace_text_in_range(None, &text));
        }
        self.sync_field();
    }

    /// The keyboard's action key: delivered as Enter, which is what the field binds.
    pub(crate) fn handle_ime_action(&self) {
        self.press(
            gpui::Keystroke {
                modifiers: Modifiers::default(),
                key: "enter".into(),
                key_char: Some("\n".into()),
            },
            false,
        );
    }

    pub(crate) fn handle_ime_state(&self, edited: Field, serial: i32) {
        self.ime_serial.set(serial);
        let before = self.field.borrow().clone();
        let Some(before) = before else {
            self.sync_field();
            return;
        };
        self.input_handler.with(|handler| text_input::apply(handler, &before, &edited));
        let now = self.input_handler.with(Field::read).flatten();
        if let Some(now) = &now
            && *now != edited
        {
            now.send(serial);
        }
        self.field.replace(now);
        self.schedule_frame();
    }

    /// Sends GPUI's field to the keyboard when it differs from the keyboard's copy.
    fn sync_field(&self) {
        let now = self.input_handler.with(Field::read).flatten();
        let changed = *self.field.borrow() != now;
        if changed {
            if let Some(now) = &now {
                now.send(self.ime_serial.get());
            }
            self.field.replace(now);
        }
    }

    pub(crate) fn handle_back(&self) {
        self.callbacks.back.with(|callback| callback());
    }
}

impl rwh::HasWindowHandle for AndroidWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let surface = self.0.surface.borrow();
        let surface = surface.as_ref().ok_or(rwh::HandleError::Unavailable)?;
        let handle = rwh::AndroidNdkWindowHandle::new(surface.0.ptr().cast());
        // SAFETY: the handle names the surface GPUI currently draws into.
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl rwh::HasDisplayHandle for AndroidWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        Ok(rwh::DisplayHandle::android())
    }
}

impl PlatformWindow for AndroidWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: Point::default(),
            size: self.0.content_size(),
        }
    }

    fn is_maximized(&self) -> bool {
        true
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.content_size()
    }

    fn resize(&mut self, _size: Size<Pixels>) {}

    fn scale_factor(&self) -> f32 {
        self.0.scale.get()
    }

    fn appearance(&self) -> WindowAppearance {
        if self.0.dark.get() { WindowAppearance::Dark } else { WindowAppearance::Light }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(self.0.display()))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.0.mouse_position.get()
    }

    fn modifiers(&self) -> Modifiers {
        self.0.modifiers.get()
    }

    fn capslock(&self) -> Capslock {
        Capslock { on: self.0.capslock.get() }
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.input_handler.set(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {}

    fn is_active(&self) -> bool {
        self.0.active.get()
    }

    fn visibility(&self) -> WindowVisibility {
        self.0.visibility.get()
    }

    fn is_hovered(&self) -> bool {
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, _title: &str) {}

    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {}

    /// Android's own Back on a root screen: the task goes behind, its state kept.
    fn minimize(&self) {
        bridge::move_to_back();
    }

    fn zoom(&self) {}

    fn toggle_fullscreen(&self) {}

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn frame_waker(&self) -> Option<Rc<dyn Fn()>> {
        let window = Rc::downgrade(&self.0);
        Some(Rc::new(move || {
            if let Some(window) = window.upgrade() {
                window.schedule_frame();
            }
        }))
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.callbacks.request_frame.set(callback);
        self.0.redraw_all();
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.0.callbacks.input.set(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.active.set(callback);
    }

    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.0.callbacks.visibility.set(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.hover.set(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.callbacks.resize.set(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.moved.set(callback);
    }

    fn on_display_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.display_changed.set(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.callbacks.should_close.set(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.0.callbacks.hit_test.set(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.callbacks.close.set(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.appearance.set(callback);
    }

    fn draw(&self, scene: &Scene) {
        let mut renderer = self.0.renderer.borrow_mut();
        if renderer.device_lost()
            && let Some(surface) = self.0.surface.borrow().clone()
            && let Err(error) = renderer.recover(&surface)
        {
            log::error!("gpui_android: the GPU could not recover: {error:#}");
            return;
        }
        let started = Instant::now();
        renderer.draw(scene);
        let counts = [scene.quads.len(), scene.shadows.len(), scene.paths.len(), scene.monochrome_sprites.len(), scene.polychrome_sprites.len()];
        self.0.drawn.set(Some((started.elapsed(), renderer.frame_timing(), counts)));
        static FIRST: std::sync::Once = std::sync::Once::new();
        FIRST.call_once(|| log::info!("gpui_android: first frame drawn"));
        // Once, on the first frame with something drawn, when the app asks for it.
        static TESTED: std::sync::Once = std::sync::Once::new();
        if scene.len() > 0 && std::env::var_os("VECTOR_GPU_SELF_TEST").is_some() {
            TESTED.call_once(|| {
                log::info!("gpui_android: GPU self-test at {:?}", renderer.viewport_size());
                for line in renderer.gpu_self_test(scene) {
                    log::info!("gpui_android: GPU self-test, {line}");
                }
            });
        }
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.0.renderer.borrow().sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.0.renderer.borrow().gpu_specs()
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {}

    fn insets(&self) -> WindowInsets {
        self.0.current_insets()
    }

    fn on_insets_changed(&self, callback: Box<dyn FnMut(WindowInsets)>) {
        self.0.callbacks.insets.set(callback);
    }

    fn set_back_handler(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.back.set(callback);
    }

    fn set_back_enabled(&self, enabled: bool) {
        bridge::set_back_enabled(enabled);
    }

    fn show_soft_keyboard(&self) {
        bridge::show_keyboard(true);
    }

    fn hide_soft_keyboard(&self) {
        bridge::show_keyboard(false);
    }

    fn text_input_state_changed(&self, change: TextInputStateChange) {
        match change {
            TextInputStateChange::FocusLost => {
                self.0.field.replace(None);
                Field::default().send(self.0.ime_serial.get());
            }
            _ => self.0.sync_field(),
        }
    }

    fn set_text_input_configuration(&mut self, configuration: TextInputConfiguration) {
        bridge::set_input_options(text_input::options(&configuration));
    }
}

impl Drop for AndroidWindow {
    fn drop(&mut self) {
        self.0.renderer.borrow_mut().destroy();
    }
}
