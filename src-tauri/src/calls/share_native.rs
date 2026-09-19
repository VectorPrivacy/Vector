//! The shared screen's sound on macOS, where the webview's picker gives none:
//! ScreenCaptureKit captures the display's audio with this process left out, so
//! the voices never enter the capture at all, and hands the samples to the same
//! share input the webview's tap would. Picture-free: the stream is told to make
//! the smallest, slowest frames it can and nothing listens for them.

#[cfg(target_os = "macos")]
mod imp {
    use crate::calls::media::ShareInput;
    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass};
    use objc2_core_audio_types::{kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, AudioBufferList};
    use objc2_core_foundation::CFRetained;
    use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMSampleBuffer, CMTime};
    use objc2_foundation::{NSArray, NSError};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput,
        SCStreamOutputType,
    };
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    struct Ivars {
        share: Arc<ShareInput>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = AnyThread]
        #[name = "VectorShareTap"]
        #[ivars = Ivars]
        struct ShareTap;

        unsafe impl NSObjectProtocol for ShareTap {}

        unsafe impl SCStreamOutput for ShareTap {
            #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
            #[allow(non_snake_case)]
            unsafe fn stream_didOutputSampleBuffer_ofType(
                &self,
                _stream: &SCStream,
                sample_buffer: &CMSampleBuffer,
                r#type: SCStreamOutputType,
            ) {
                if r#type != SCStreamOutputType::Audio {
                    return;
                }
                if let Some((rate, channels, samples)) = pcm_of(sample_buffer) {
                    self.ivars().share.push(rate, channels, &samples);
                }
            }
        }

        unsafe impl SCStreamDelegate for ShareTap {
            #[unsafe(method(stream:didStopWithError:))]
            #[allow(non_snake_case)]
            unsafe fn stream_didStopWithError(&self, _stream: &SCStream, error: &NSError) {
                log_warn!("[CALLS] Screen audio capture stopped: {error}");
                self.ivars().share.active.store(false, Ordering::Relaxed);
            }
        }
    );

    impl ShareTap {
        fn new(share: Arc<ShareInput>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { share });
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Interleaved f32 samples out of one audio sample buffer, whatever its layout.
    unsafe fn pcm_of(sb: &CMSampleBuffer) -> Option<(u32, u8, Vec<f32>)> {
        let desc = sb.format_description()?;
        let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(&desc);
        if asbd.is_null() {
            return None;
        }
        let asbd = &*asbd;
        if asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0 || asbd.mBitsPerChannel != 32 {
            return None;
        }
        let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
        let channels = asbd.mChannelsPerFrame.clamp(1, 2) as u8;
        let rate = asbd.mSampleRate as u32;

        let mut needed = 0usize;
        let status = sb.audio_buffer_list_with_retained_block_buffer(&mut needed, std::ptr::null_mut(), 0, None, None, 0, std::ptr::null_mut());
        if status != 0 && status != -12737 || needed == 0 {
            // -12737 is the "buffer too small" answer to a size query.
            if status != -12737 {
                return None;
            }
        }
        let mut raw = vec![0u64; needed.div_ceil(8)];
        let list = raw.as_mut_ptr() as *mut AudioBufferList;
        let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
        let status = sb.audio_buffer_list_with_retained_block_buffer(std::ptr::null_mut(), list, needed, None, None, 0, &mut block);
        if status != 0 {
            return None;
        }
        // The block buffer is retained for us; release it once the samples are copied out.
        let _block = CFRetained::from_raw(std::ptr::NonNull::new(block)?);
        let count = (*list).mNumberBuffers as usize;
        let buffers = std::slice::from_raw_parts((*list).mBuffers.as_ptr(), count);
        let out = if planar {
            let planes: Vec<&[f32]> = buffers
                .iter()
                .take(channels as usize)
                .map(|b| std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4))
                .collect();
            let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
            let mut v = Vec::with_capacity(frames * channels as usize);
            for i in 0..frames {
                for p in &planes {
                    v.push(p[i]);
                }
            }
            v
        } else {
            let b = buffers.first()?;
            std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4).to_vec()
        };
        Some((rate, channels, out))
    }

    struct Running {
        stream: Retained<SCStream>,
        _tap: Retained<ShareTap>,
    }
    // ScreenCaptureKit drives these from its own queues; we only start and stop them.
    unsafe impl Send for Running {}

    /// A retained object crossing a channel between our own threads.
    struct Carried<T>(T);
    unsafe impl<T> Send for Carried<T> {}

    static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

    /// ScreenCaptureKit exists from macOS 12.3; audio capture from 13.
    pub fn available() -> bool {
        AnyClass::get(c"SCStreamConfiguration").is_some()
    }

    /// Everything Objective-C happens on one plain thread, so nothing that cannot
    /// cross threads is ever held across an await.
    pub async fn start(share: Arc<ShareInput>) -> Result<(), String> {
        if !available() {
            return Err("Screen audio needs macOS 13 or later".into());
        }
        stop().await;
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("calls-share-native".into())
            .spawn(move || {
                let _ = tx.send(start_sync(share));
            })
            .map_err(|e| e.to_string())?;
        rx.await.map_err(|_| "Screen audio setup was interrupted".to_string())?
    }

    fn start_sync(share: Arc<ShareInput>) -> Result<(), String> {
        // The display list comes back on a block; the first ask also raises the
        // system's screen recording prompt.
        let (tx, rx) = std::sync::mpsc::channel::<Carried<Result<Retained<SCShareableContent>, String>>>();
        let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if content.is_null() {
                Err(if error.is_null() { "no shareable content".to_string() } else { unsafe { (*error).localizedDescription().to_string() } })
            } else {
                unsafe { Retained::retain(content) }.ok_or_else(|| "no shareable content".to_string())
            };
            let _ = tx.send(Carried(result));
        });
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
        let content = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .map_err(|_| "Screen Recording permission was not granted".to_string())?
            .0?;

        let running = unsafe {
            let displays = content.displays();
            let display = displays.firstObject().ok_or("No display to capture")?;
            let filter = SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new());
            let config = SCStreamConfiguration::new();
            config.setCapturesAudio(true);
            config.setExcludesCurrentProcessAudio(true);
            config.setSampleRate(48_000);
            config.setChannelCount(2);
            // No picture wanted: the cheapest frames the stream will agree to make.
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime::new(1, 1));
            config.setShowsCursor(false);
            let tap = ShareTap::new(Arc::clone(&share));
            let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*tap);
            let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(delegate));
            let output: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*tap);
            let queue = DispatchQueue::new("io.vectorapp.share-audio", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(output, SCStreamOutputType::Audio, Some(&queue))
                .map_err(|e| e.localizedDescription().to_string())?;
            Running { stream, _tap: tap }
        };

        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let done = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send(if error.is_null() { Ok(()) } else { Err(unsafe { (*error).localizedDescription().to_string() }) });
        });
        unsafe { running.stream.startCaptureWithCompletionHandler(Some(&done)) };
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| "Screen audio did not start".to_string())??;
        share.active.store(true, Ordering::Relaxed);
        *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) = Some(running);
        Ok(())
    }

    pub async fn stop() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("calls-share-native-stop".into())
            .spawn(move || {
                stop_sync();
                let _ = tx.send(());
            });
        if spawned.is_ok() {
            let _ = rx.await;
        }
    }

    fn stop_sync() {
        let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(running) = running else { return };
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let done = RcBlock::new(move |_error: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { running.stream.stopCaptureWithCompletionHandler(Some(&done)) };
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
        drop(running);
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use crate::calls::media::ShareInput;
    use std::sync::Arc;

    pub fn available() -> bool {
        false
    }
    pub async fn start(_share: Arc<ShareInput>) -> Result<(), String> {
        Err("Native screen audio is not available on this platform".into())
    }
    pub async fn stop() {}
}

pub use imp::{available, start, stop};
