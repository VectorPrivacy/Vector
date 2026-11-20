use crate::CallbackSlot;
use crate::bridge::{self, Event, lifecycle};
use crate::dispatcher::{self, AndroidDispatcher};
use crate::display::AndroidDisplay;
use crate::text_input::Field;
use crate::window::{AndroidWindow, WindowState};
use anyhow::anyhow;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, ClipboardItem, CursorStyle,
    DummyKeyboardMapper, ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Platform,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, Result, RunnableVariant, Task, ThermalState,
    WindowAppearance, WindowParams, px, size,
};
use gpui_wgpu::{CosmicTextSystem, GpuContext};
use ndk::looper::{FdEvent, ThreadLooper};
use ndk::native_window::NativeWindow;
use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub density: f32,
    pub dark: bool,
    pub refresh_hz: f32,
}

pub struct AndroidPlatform(Rc<PlatformState>);

pub(crate) struct PlatformState {
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<CosmicTextSystem>,
    main_receiver: PriorityQueueReceiver<RunnableVariant>,
    main_wake: Arc<OwnedFd>,
    pub gpu: GpuContext,
    pub config: Cell<Config>,
    pub insets: Cell<([f32; 4], f32)>,
    surface: RefCell<Option<NativeWindow>>,
    window: RefCell<Weak<WindowState>>,
    active_window: Cell<Option<AnyWindowHandle>>,
    quitting: Cell<bool>,
    finish_launching: RefCell<Option<Box<dyn FnOnce()>>>,
    pending_urls: RefCell<Vec<String>>,
    open_urls: CallbackSlot<Box<dyn FnMut(Vec<String>)>>,
    quit: CallbackSlot<Box<dyn FnMut() -> bool>>,
    app_lifecycle: CallbackSlot<Box<dyn FnMut(AppLifecyclePhase)>>,
    memory_warning: CallbackSlot<Box<dyn FnMut()>>,
    displays_changed: CallbackSlot<Box<dyn FnMut()>>,
}

struct AndroidKeyboardLayout;

impl PlatformKeyboardLayout for AndroidKeyboardLayout {
    fn id(&self) -> &str {
        "android-default"
    }

    fn name(&self) -> &str {
        "Android Default"
    }
}

impl AndroidPlatform {
    /// On GPUI's thread, which the dispatcher takes as the main one.
    pub fn new(_headless: bool) -> Self {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let main_wake = Arc::new(bridge::new_eventfd());
        let dispatcher = Arc::new(AndroidDispatcher::new(main_sender, main_wake.clone()));
        Self(Rc::new(PlatformState {
            background_executor: BackgroundExecutor::new(dispatcher.clone()),
            foreground_executor: ForegroundExecutor::new(dispatcher),
            text_system: Arc::new(CosmicTextSystem::new("Roboto")),
            main_receiver,
            main_wake,
            gpu: Rc::new(RefCell::new(None)),
            config: Cell::new(Config {
                density: 1.,
                dark: false,
                refresh_hz: 60.,
            }),
            insets: Cell::new(([0.; 4], 0.)),
            surface: RefCell::new(None),
            window: RefCell::new(Weak::new()),
            active_window: Cell::new(None),
            quitting: Cell::new(false),
            finish_launching: RefCell::new(None),
            pending_urls: RefCell::new(Vec::new()),
            open_urls: CallbackSlot::default(),
            quit: CallbackSlot::default(),
            app_lifecycle: CallbackSlot::default(),
            memory_warning: CallbackSlot::default(),
            displays_changed: CallbackSlot::default(),
        }))
    }
}

impl PlatformState {
    fn window(&self) -> Option<Rc<WindowState>> {
        self.window.borrow().upgrade()
    }

    fn lifecycle(&self, phase: AppLifecyclePhase) {
        self.app_lifecycle.with(|callback| callback(phase));
    }

    fn launch_if_ready(&self) {
        if self.surface.borrow().is_none() {
            return;
        }
        let launch = self.finish_launching.borrow_mut().take();
        if let Some(launch) = launch {
            launch();
        }
    }

    fn handle(&self, event: Event) {
        let window = self.window();
        match event {
            Event::SurfaceCreated(surface) => {
                log::info!("gpui_android: surface {}x{}", surface.width(), surface.height());
                if let Some(window) = &window {
                    window.attach_surface(surface.clone());
                }
                self.surface.replace(Some(surface));
                self.launch_if_ready();
            }
            Event::SurfaceChanged { width, height } => {
                if let Some(window) = &window {
                    window.set_surface_size(width, height);
                }
            }
            Event::SurfaceDestroyed(done) => {
                if let Some(window) = &window {
                    window.detach_surface();
                }
                self.surface.replace(None);
                let _ = done.send(());
            }
            Event::Insets { safe, ime } => {
                self.insets.set((safe, ime));
                if let Some(window) = &window {
                    window.set_insets(safe, ime);
                }
            }
            Event::Configuration { density, dark } => {
                let mut config = self.config.get();
                config.density = density;
                config.dark = dark;
                self.config.set(config);
                if let Some(window) = &window {
                    window.set_configuration(density, dark);
                }
                self.displays_changed.with(|callback| callback());
            }
            Event::Touch { id, phase, x, y, pressure, time } => {
                if let Some(window) = &window {
                    window.handle_touch(id, phase, x, y, pressure, time);
                }
            }
            Event::Key { down, key_code, unicode, base, meta, repeat } => {
                if let Some(window) = &window {
                    window.handle_key(down, key_code, unicode, base, meta, repeat);
                }
            }
            Event::Lifecycle(phase) => match phase {
                lifecycle::STARTED => {
                    self.lifecycle(AppLifecyclePhase::Foreground);
                    if let Some(window) = &window {
                        window.set_visible(true);
                    }
                }
                lifecycle::RESUMED => {
                    self.lifecycle(AppLifecyclePhase::Active);
                    if let Some(window) = &window {
                        window.set_active(true);
                    }
                }
                lifecycle::PAUSED => {
                    if let Some(window) = &window {
                        window.set_active(false);
                    }
                    self.lifecycle(AppLifecyclePhase::Inactive);
                }
                lifecycle::STOPPED => {
                    if let Some(window) = &window {
                        window.set_visible(false);
                    }
                    self.lifecycle(AppLifecyclePhase::Background);
                }
                lifecycle::DESTROYED => self.lifecycle(AppLifecyclePhase::Disconnected),
                _ => {}
            },
            Event::Focus(focused) => {
                if let Some(window) = &window {
                    window.set_active(focused);
                }
            }
            Event::Back => {
                if let Some(window) = &window {
                    window.handle_back();
                }
            }
            // ComponentCallbacks2: 20 only means the UI went out of sight.
            Event::TrimMemory(level) if level >= 10 && level != 20 => {
                self.memory_warning.with(|callback| callback());
            }
            Event::TrimMemory(_) => {}
            Event::OpenUrl(url) => {
                if self.open_urls.with(|callback| callback(vec![url.clone()])).is_none() {
                    self.pending_urls.borrow_mut().push(url);
                }
            }
            Event::ImeState { text, selection, composing, serial } => {
                if let Some(window) = &window {
                    window.handle_ime_state(Field::from_keyboard(&text, selection, composing), serial);
                }
            }
            Event::ImeAction => {
                if let Some(window) = &window {
                    window.handle_ime_action();
                }
            }
        }
    }
}

impl Platform for AndroidPlatform {
    fn platform_info(&self) -> gpui::PlatformInfo {
        gpui::PlatformInfo {
            os: gpui::OperatingSystem::Android,
            target: gpui::PlatformTarget::Native,
        }
    }

    fn background_executor(&self) -> BackgroundExecutor {
        self.0.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.0.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.0.text_system.clone()
    }

    /// Runs the looper until quit. Launching waits for the first surface, so the app's first
    /// window always has one to draw into.
    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        let looper = ThreadLooper::prepare();
        let mut receiver = self.0.main_receiver.clone();
        // SAFETY: the eventfd lives in the platform state, which outlives this loop.
        let main_fd = unsafe { BorrowedFd::borrow_raw(self.0.main_wake.as_raw_fd()) };
        let registered = looper.add_fd_with_callback(main_fd, FdEvent::INPUT, move |fd, _| {
            bridge::clear_wake(fd.as_raw_fd());
            loop {
                match receiver.try_pop() {
                    Ok(Some(runnable)) => dispatcher::run(runnable),
                    Ok(None) => return true,
                    Err(_) => return false,
                }
            }
        });
        if registered.is_err() {
            log::error!("gpui_android: could not watch the main queue");
            return;
        }
        let state = self.0.clone();
        // SAFETY: the bridge's eventfd lives for the process.
        let event_fd = unsafe { BorrowedFd::borrow_raw(bridge::wake_fd()) };
        let registered = looper.add_fd_with_callback(event_fd, FdEvent::INPUT, move |fd, _| {
            bridge::clear_wake(fd.as_raw_fd());
            for event in bridge::take_events() {
                state.handle(event);
            }
            true
        });
        if registered.is_err() {
            log::error!("gpui_android: could not watch the bridge");
            return;
        }
        // SAFETY: the frame eventfd lives for the process.
        let frame_fd = unsafe { BorrowedFd::borrow_raw(crate::window::frame_wake_fd()) };
        let registered = looper.add_fd_with_callback(frame_fd, FdEvent::INPUT, |fd, _| {
            bridge::clear_wake(fd.as_raw_fd());
            crate::window::draw_pending(true);
            true
        });
        if registered.is_err() {
            log::error!("gpui_android: could not watch the frame wake");
            return;
        }
        self.0.finish_launching.replace(Some(on_finish_launching));
        // Events queued before the looper watched them.
        for event in bridge::take_events() {
            self.0.handle(event);
        }
        self.0.launch_if_ready();
        while !self.0.quitting.get() {
            if looper.poll_once().is_err() {
                break;
            }
        }
    }

    fn quit(&self) {
        self.0.quit.with(|callback| callback());
        self.0.quitting.set(true);
        bridge::finish();
    }

    fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {}

    fn activate(&self, _ignoring_other_apps: bool) {}

    fn hide(&self) {}

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.primary_display().into_iter().collect()
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let display = match self.0.window() {
            Some(window) => window.display(),
            None => {
                let config = self.0.config.get();
                let (width, height) = self
                    .0
                    .surface
                    .borrow()
                    .as_ref()
                    .map_or((0, 0), |s| (s.width(), s.height()));
                AndroidDisplay {
                    size: size(px(width as f32 / config.density), px(height as f32 / config.density)),
                    refresh_hz: config.refresh_hz,
                }
            }
        };
        Some(Rc::new(display))
    }

    fn on_displays_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.displays_changed.set(callback);
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.0.active_window.get()
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        _options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        let surface = self
            .0
            .surface
            .borrow()
            .clone()
            .ok_or_else(|| anyhow!("Android has no surface to open a window in"))?;
        let window = AndroidWindow::new(handle, &self.0, surface)?;
        self.0.window.replace(Rc::downgrade(&window.0));
        self.0.active_window.set(Some(handle));
        Ok(Box::new(window))
    }

    fn window_appearance(&self) -> WindowAppearance {
        if self.0.config.get().dark { WindowAppearance::Dark } else { WindowAppearance::Light }
    }

    fn open_url(&self, url: &str) {
        bridge::open_url(url);
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.0.open_urls.set(callback);
        let pending = std::mem::take(&mut *self.0.pending_urls.borrow_mut());
        if !pending.is_empty() {
            let state = Rc::downgrade(&self.0);
            self.0
                .foreground_executor
                .spawn(async move {
                    if let Some(state) = state.upgrade() {
                        state.open_urls.with(|callback| callback(pending));
                    }
                })
                .detach();
        }
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        if options.directories && !options.files {
            let _ = tx.send(Err(anyhow!("Folders can't be picked on Android")));
            return rx;
        }
        let extensions: Vec<String> = options
            .filters
            .iter()
            .flatten()
            .flat_map(|(_, exts)| exts.iter().map(|e| e.to_string()))
            .collect();
        bridge::pick_files(options.multiple, &extensions, tx);
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(anyhow!("The Android save dialog is not wired yet")));
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    // A phone has no file manager to reveal a file in: it opens in the app that takes its type.
    fn reveal_path(&self, path: &Path) {
        bridge::open_file(path);
    }

    fn open_with_system(&self, path: &Path) {
        bridge::open_file(path);
    }

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.quit.set(callback);
    }

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}

    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}

    fn on_system_sleep(&self, _callback: Box<dyn FnMut()>) {}

    fn prevent_idle_sleep(&self, _reason: &str) -> Task<Result<gpui::ActivityGuard>> {
        Task::ready(Err(anyhow!("Idle sleep prevention is not wired on Android yet")))
    }

    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.0.app_lifecycle.set(callback);
    }

    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.0.memory_warning.set(callback);
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn app_path(&self) -> Result<PathBuf> {
        Ok(std::env::current_exe()?)
    }

    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        Err(anyhow!("Android apps carry no auxiliary executables ({name})"))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}

    fn hide_cursor_until_mouse_moves(&self) {}

    fn is_cursor_visible(&self) -> bool {
        false
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        if let Some(text) = item.text() {
            bridge::set_clipboard(&text);
        }
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        bridge::clipboard().map(ClipboardItem::new_string)
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow!("Credentials are not wired on Android yet")))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(AndroidKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}
}
