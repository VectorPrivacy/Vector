/**
 * Voice recording functionality with Mobile-first UX:
 * - Hold to record (200ms threshold)
 * - Drag up to lock recording
 * - Drag left to cancel
 * - Preview before sending
 *
 * Audio playback uses the unified Rust cpal audio engine:
 * - No <audio> elements or Web Audio API (fixes Linux WebKitGTK audio bugs)
 * - Precomputed FFT waveform data from Rust (no real-time IPC)
 * - Notification sounds mix with voice playback (desktop)
 */

const HOLD_THRESHOLD_MS = 200;
const LOCK_DRAG_THRESHOLD = 85; // pixels to drag up to lock
const LOCK_SHOW_THRESHOLD = 5; // pixels to drag up before lock indicator appears
const CANCEL_DRAG_THRESHOLD = 100; // pixels to drag left to cancel

/**
 * Recording states
 */
const RecordingState = {
    IDLE: 'idle',
    PENDING: 'pending', // Waiting for hold threshold
    RECORDING: 'recording',
    LOCKED: 'locked', // Recording locked (finger released)
    PREVIEW: 'preview', // Showing audio preview
    CANCELLED: 'cancelled'
};

class VoiceRecorder {
    /**
     * @param {HTMLElement} button - The recording button element
     * @param {HTMLElement} inputContainer - The chat input container element
     * @param {HTMLElement} sendButton - The send button element
     */
    constructor(button, inputContainer, sendButton) {
        this.button = button;
        this.inputContainer = inputContainer;
        this.sendButton = sendButton;
        this.state = RecordingState.IDLE;
        this.holdTimer = null;
        this.startPosition = { x: 0, y: 0 };
        this.currentPosition = { x: 0, y: 0 };
        this.recordingStartTime = null;
        this.timerInterval = null;
        this.isStoppingRecording = false; // Lock to prevent double stop_recording calls
        this.dragAxis = null; // 'x' for cancel, 'y' for lock - locked once determined
        this.lastMoveAxis = null; // Track previous axis for smooth transitions
        this.tooltipTimeout = null; // Timeout for hiding the "Hold to record" tooltip

        // Audio engine state (replaces audioData + audioElement)
        this.previewSourceId = null;
        this.waveformData = null;     // Uint8Array from Rust
        this.waveformFps = 30;
        this.waveformBins = 64;
        this.durationMs = 0;
        this.playStartTime = 0;       // performance.now() when play started
        this.playStartPos = 0;        // position_ms when play started
        this.previewAnimationId = null;

        // Callbacks
        this.onSend = null;
        this.onCancel = null;
        this.onStateChange = null;

        this.isPlaying = false;
        this._bindEvents();
    }

    /** The waveform the seek gesture measures; VoiceRecorderUI binds it. */
    get previewWaveform() { return VectorSvelte.voiceEls().waveform; }

    /**
     * Binds all event listeners
     */
    _bindEvents() {
        // Pointer events for cross-platform support
        this.button.addEventListener('pointerdown', this._onPointerDown.bind(this));
        document.addEventListener('pointermove', this._onPointerMove.bind(this));
        document.addEventListener('pointerup', this._onPointerUp.bind(this));
        document.addEventListener('pointercancel', this._onPointerUp.bind(this));

        // The dot, the preview controls and the waveform are the component's; it calls back.
        VectorSvelte.setVoiceHandlers({
            dotClick: () => this._onRedCircleClick(),
            previewDelete: () => this._onPreviewDelete(),
            previewPlayPause: () => this._onPreviewPlayPause(),
            waveformPointerDown: (e) => this._onWaveformPointerDown(e),
        });

        // Listen for audio_ended events from the engine
        this._audioEndedUnlisten = null;
    }

    /**
     * Handles play/pause button click in preview
     */
    async _onPreviewPlayPause() {
        if (!this.previewSourceId) return;

        if (this.isPlaying) {
            await this._pausePreview();
        } else {
            VectorSvelte.setVoicePreview({ playing: true });
            VectorSvelte.claimPlayback('voice-preview', () => this._pausePreview());

            try {
                const posMs = await invoke('audio_play', { id: this.previewSourceId });
                this.playStartTime = performance.now();
                this.playStartPos = posMs;
                this.isPlaying = true;
                this._startPreviewAnimation();
            } catch (err) {
                console.error('Playback failed:', err);
                VectorSvelte.setVoicePreview({ playing: false });
            }
        }
    }

    async _pausePreview() {
        VectorSvelte.releasePlayback('voice-preview');
        if (!this.isPlaying || !this.previewSourceId) return;
        try {
            await invoke('audio_pause', { id: this.previewSourceId });
        } catch (err) {
            console.error('Pause failed:', err);
        }
        this.isPlaying = false;
        this._stopPreviewAnimation();
        VectorSvelte.setVoicePreview({ playing: false });
    }

    /**
     * Handles pointer down on the mic button
     */
    _onPointerDown(e) {
        if (this.state !== RecordingState.IDLE) return;

        e.preventDefault();
        e.stopPropagation();

        // Store the pointer ID for tracking
        this.activePointerId = e.pointerId;

        // Set pointer capture on the button to receive all pointer events
        this.button.setPointerCapture(e.pointerId);

        this.startPosition = { x: e.clientX, y: e.clientY };
        this.currentPosition = { x: e.clientX, y: e.clientY };

        this._setState(RecordingState.PENDING);

        // Start hold timer
        this.holdTimer = setTimeout(async () => {
            await this._startRecording();
        }, HOLD_THRESHOLD_MS);
    }

    /**
     * Handles pointer move during recording
     */
    _onPointerMove(e) {
        if (this.state !== RecordingState.RECORDING && this.state !== RecordingState.PENDING) return;

        // Only track the active pointer
        if (this.activePointerId !== undefined && e.pointerId !== this.activePointerId) return;

        e.preventDefault();

        this.currentPosition = { x: e.clientX, y: e.clientY };

        const deltaX = this.startPosition.x - this.currentPosition.x;
        const deltaY = this.startPosition.y - this.currentPosition.y;
        const absDeltaX = Math.abs(deltaX);
        const absDeltaY = Math.abs(deltaY);

        // Reset axis if user returns close to origin (within 15px)
        // This allows changing direction after returning to center
        if (this.dragAxis && absDeltaX < 15 && absDeltaY < 15) {
            this.dragAxis = null;
        }

        // Determine primary axis based on which delta is larger
        // Lock axis after a small movement threshold (15px)
        if (!this.dragAxis) {
            if (absDeltaX > 15 || absDeltaY > 15) {
                this.dragAxis = absDeltaX > absDeltaY ? 'x' : 'y';
            }
        }

        // Check for cancel gesture (drag left) - only if on X axis
        if (this.dragAxis === 'x' && deltaX > CANCEL_DRAG_THRESHOLD) {
            this._cancelRecording();
            return;
        }

        // Check for lock gesture (drag up) - only if on Y axis
        if (this.dragAxis === 'y' && deltaY > LOCK_DRAG_THRESHOLD && this.state === RecordingState.RECORDING) {
            this._lockRecording();
            return;
        }

        // Update visual feedback
        this._updateDragFeedback(deltaX, deltaY);
    }

    /**
     * Handles pointer up
     */
    async _onPointerUp(e) {
        // Only handle the active pointer
        if (this.activePointerId !== undefined && e.pointerId !== this.activePointerId) return;

        // Clear the active pointer and drag axis
        this.activePointerId = undefined;
        this._resetDragState();

        if (this.state === RecordingState.PENDING) {
            // Didn't hold long enough - show tooltip hint
            clearTimeout(this.holdTimer);
            this._showTooltip();
            this._setState(RecordingState.IDLE);
            return;
        }

        if (this.state === RecordingState.RECORDING) {
            // Stop and show preview
            await this._stopRecording();
        }

        // If locked, do nothing - wait for red circle click
    }

    /**
     * Shows the "Hold to record" tooltip briefly
     */
    _showTooltip() {
        if (this.tooltipTimeout) {
            clearTimeout(this.tooltipTimeout);
        }

        VectorSvelte.setVoiceTooltip(true);

        this.tooltipTimeout = setTimeout(() => {
            VectorSvelte.setVoiceTooltip(false);
            this.tooltipTimeout = null;
        }, 2000);
    }

    /**
     * Resets drag state (axis lock and red circle position)
     */
    _resetDragState() {
        this.dragAxis = null;
        this.lastMoveAxis = null;
        // `returning` eases the dot back to origin; it lifts once that animation is done.
        VectorSvelte.setVoiceDot({ returning: true, transform: '' });
        setTimeout(() => VectorSvelte.setVoiceDot({ returning: false }), 200);
    }

    /**
     * Starts the actual recording
     */
    async _startRecording() {
        try {
            // Reset status text for new recording
            VectorSvelte.setVoiceStatusText('Recording...');
            await invoke('start_recording');
            this._setState(RecordingState.RECORDING);
            this.recordingStartTime = Date.now();
            this._startTimer();
        } catch (err) {
            console.error('Recording start failed:', err);
            await popupConfirm('Recording Error', err, true, '', 'vector_warning.svg');
            this._setState(RecordingState.IDLE);
        }
    }

    /**
     * Stops recording and shows preview via audio engine
     */
    async _stopRecording() {
        // Prevent double calls while stopping
        if (this.isStoppingRecording) return;
        this.isStoppingRecording = true;

        // Update status text to show we're finishing
        VectorSvelte.setVoiceStatusText('Finishing...');

        try {
            // Register event listeners BEFORE stop_recording to avoid missing
            // fast FFT events (voice recordings are short WAVs)
            this._listenForEnded();
            this._listenForWaveform();

            // stop_recording returns { id, duration_ms, waveform_fps, bins }
            // Waveform data arrives async via audio_waveform event
            const result = await invoke('stop_recording');
            this._stopTimer();

            this.previewSourceId = result.id;
            this.durationMs = result.duration_ms;
            this.waveformData = null; // arrives via event
            this.waveformFps = result.waveform_fps;
            this.waveformBins = result.bins;

            this._setState(RecordingState.PREVIEW);
        } catch (err) {
            console.error('Recording stop failed:', err);
            this._setState(RecordingState.IDLE);
        } finally {
            this.isStoppingRecording = false;
        }
    }

    /**
     * Listen for audio_ended event from the engine
     */
    async _listenForEnded() {
        if (this._audioEndedUnlisten) {
            this._audioEndedUnlisten();
            this._audioEndedUnlisten = null;
        }
        this._audioEndedUnlisten = await window.__TAURI__.event.listen('audio_ended', (event) => {
            if (this.previewSourceId && event.payload.id === this.previewSourceId) {
                this._onPreviewEnded();
            }
        });
    }

    /**
     * Listen for waveform data from the engine (computed in background)
     */
    async _listenForWaveform() {
        if (this._audioWaveformUnlisten) {
            this._audioWaveformUnlisten();
            this._audioWaveformUnlisten = null;
        }
        this._audioWaveformUnlisten = await window.__TAURI__.event.listen('audio_waveform', (event) => {
            if (this.previewSourceId && event.payload.id === this.previewSourceId) {
                this.waveformData = new Uint8Array(event.payload.waveform);
                this.waveformFps = event.payload.waveform_fps;
                this.waveformBins = event.payload.bins;
                if (this._audioWaveformUnlisten) { this._audioWaveformUnlisten(); this._audioWaveformUnlisten = null; }
            }
        });
    }

    /**
     * Locks the recording (allows releasing finger)
     */
    _lockRecording() {
        // Reset drag state (red circle returns to origin)
        this._resetDragState();

        // The lock target fades out before the locked state takes over.
        VectorSvelte.setVoiceLockFading(true);
        setTimeout(() => this._setState(RecordingState.LOCKED), 200); // Match the CSS transition duration
    }

    /**
     * Cancels the current recording
     */
    async _cancelRecording() {
        // Prevent double calls while stopping
        if (this.isStoppingRecording) return;

        // Reset drag state
        this._resetDragState();

        if (this.state === RecordingState.RECORDING || this.state === RecordingState.LOCKED) {
            this.isStoppingRecording = true;
            try {
                const result = await invoke('stop_recording');
                // Stop the engine source that was created
                if (result && result.id) {
                    await invoke('audio_stop', { id: result.id });
                }
            } catch (err) {
                console.error('Recording cancel failed:', err);
            } finally {
                this.isStoppingRecording = false;
            }
        }

        clearTimeout(this.holdTimer);
        this._stopTimer();
        this.previewSourceId = null;
        this.waveformData = null;
        this._setState(RecordingState.CANCELLED);

        if (this.onCancel) this.onCancel();

        // Reset to idle after animation
        setTimeout(() => {
            this._setState(RecordingState.IDLE);
        }, 300);
    }

    /**
     * Handles red circle click in locked state
     */
    async _onRedCircleClick() {
        if (this.state === RecordingState.LOCKED) {
            await this._stopRecording();
        }
    }

    /**
     * Handles delete button in preview
     */
    async _onPreviewDelete() {
        this._stopPreviewAnimation();
        VectorSvelte.releasePlayback('voice-preview');
        if (this.previewSourceId) {
            try { await invoke('audio_stop', { id: this.previewSourceId }); } catch {}
        }
        if (this._audioEndedUnlisten) { this._audioEndedUnlisten(); this._audioEndedUnlisten = null; }
        if (this._audioWaveformUnlisten) { this._audioWaveformUnlisten(); this._audioWaveformUnlisten = null; }
        this.previewSourceId = null;
        this.waveformData = null;
        this.isPlaying = false;
        this._setState(RecordingState.IDLE);
        this._animateChatInputFadeIn();
        if (this.onCancel) this.onCancel();
    }

    /**
     * Sends the recorded audio — no data crosses IPC, just a control signal.
     * Returns true if send was initiated, false otherwise.
     */
    send() {
        if (this.state !== RecordingState.PREVIEW || !this.previewSourceId) return false;

        this._stopPreviewAnimation();
        VectorSvelte.releasePlayback('voice-preview');
        if (this._audioEndedUnlisten) { this._audioEndedUnlisten(); this._audioEndedUnlisten = null; }
        if (this._audioWaveformUnlisten) { this._audioWaveformUnlisten(); this._audioWaveformUnlisten = null; }
        // Don't stop engine source here — send_recording will do it
        this.previewSourceId = null;
        this.waveformData = null;
        this.isPlaying = false;
        this._setState(RecordingState.IDLE);
        this._animateChatInputFadeIn();

        return true;
    }

    /** The composer's controls return with a fade; the box plays it. */
    _animateChatInputFadeIn() {
        VectorSvelte.voiceFadeIn();
    }

    /**
     * Updates the state and UI
     */
    _setState(newState) {
        const oldState = this.state;
        this.state = newState;
        if (newState === RecordingState.IDLE) this.isPlaying = false;
        VectorSvelte.setVoiceState(newState);

        if (this.onStateChange) {
            this.onStateChange(newState, oldState);
        }
    }

    /**
     * Updates drag visual feedback
     */
    _updateDragFeedback(deltaX, deltaY) {
        // Clamp values to prevent negative movement
        const clampedDeltaX = Math.max(0, deltaX);
        const clampedDeltaY = Math.max(0, deltaY);

        // The dot rides one rail: the locked axis, else whichever delta leads.
        let currentDominantAxis = null;
        if (clampedDeltaX > 0 || clampedDeltaY > 0) {
            currentDominantAxis = clampedDeltaX >= clampedDeltaY ? 'x' : 'y';
        }
        const moveAxis = this.dragAxis || currentDominantAxis;
        this.lastMoveAxis = moveAxis;
        let dotTransform = 'scale(1)';
        if (moveAxis === 'x' && clampedDeltaX > 0) dotTransform = `translateX(${-clampedDeltaX}px) scale(1)`;
        else if (moveAxis === 'y' && clampedDeltaY > 0) dotTransform = `translateY(${-clampedDeltaY}px) scale(1)`;

        const cancelProgress = Math.min(clampedDeltaX / CANCEL_DRAG_THRESHOLD, 1);
        const effectiveAxis = this.dragAxis || this.lastMoveAxis;
        const cancelling = effectiveAxis === 'x' && clampedDeltaX > 0;

        // A leftward drag fades the readout and pulls the hint along at half speed.
        const feedback = {
            timerOpacity: cancelling ? 1 - Math.min(clampedDeltaX / 20, 1) : 1,
            statusOpacity: cancelling ? 1 - Math.min(clampedDeltaX / 50, 1) : 1,
            slide: cancelling
                ? { offset: clampedDeltaX * 0.5, opacity: 1 - (cancelProgress * 0.5), hot: cancelProgress > 0.5 }
                : { offset: 0, opacity: 1, hot: false },
            dot: { transform: dotTransform },
        };

        // The lock target rises and grows with an upward drag past the show threshold.
        if (this.dragAxis !== 'y' || clampedDeltaY < LOCK_SHOW_THRESHOLD) {
            feedback.lock = { opacity: 0, indicatorTransform: '', arrowOpacity: null };
        } else {
            const visibleProgress = Math.min((clampedDeltaY - LOCK_SHOW_THRESHOLD) / (LOCK_DRAG_THRESHOLD - LOCK_SHOW_THRESHOLD), 1);
            feedback.lock = {
                opacity: visibleProgress,
                indicatorTransform: `translateY(${-visibleProgress * 15}px) scale(${1 + visibleProgress * 0.2})`,
                arrowOpacity: 1 - visibleProgress,
            };
        }
        VectorSvelte.setVoiceDrag(feedback);
    }

    /**
     * Starts the recording timer
     */
    _startTimer() {
        this.timerInterval = setInterval(() => {
            const elapsed = Math.floor((Date.now() - this.recordingStartTime) / 1000);
            const minutes = Math.floor(elapsed / 60);
            const seconds = elapsed % 60;
            VectorSvelte.setVoiceTimer(`${minutes}:${seconds.toString().padStart(2, '0')}`);
        }, 100);
    }

    /**
     * Stops the recording timer
     */
    _stopTimer() {
        if (this.timerInterval) {
            clearInterval(this.timerInterval);
            this.timerInterval = null;
        }
    }

    /**
     * Handles pointer down on waveform for seeking
     */
    _onWaveformPointerDown(e) {
        e.preventDefault();
        this.isSeeking = true;
        this._seekThrottleTimer = null;
        this._pendingSeekMs = null;

        this.previewWaveform.setPointerCapture(e.pointerId);
        this._seekToPosition(e.clientX);

        const onMove = (moveEvent) => {
            this._seekToPosition(moveEvent.clientX);
        };

        const onUp = (upEvent) => {
            this.isSeeking = false;
            // Fire final seek at exact release position
            if (this._pendingSeekMs != null) {
                invoke('audio_seek', { id: this.previewSourceId, positionMs: this._pendingSeekMs }).catch(() => {});
                if (this.isPlaying) {
                    this.playStartTime = performance.now();
                    this.playStartPos = this._pendingSeekMs;
                }
                this._pendingSeekMs = null;
            }
            if (this._seekThrottleTimer) {
                clearTimeout(this._seekThrottleTimer);
                this._seekThrottleTimer = null;
            }
            try {
                this.previewWaveform.releasePointerCapture(upEvent.pointerId);
            } catch (err) {}
            document.removeEventListener('pointermove', onMove);
            document.removeEventListener('pointerup', onUp);
            document.removeEventListener('pointercancel', onUp);
        };

        document.addEventListener('pointermove', onMove);
        document.addEventListener('pointerup', onUp);
        document.addEventListener('pointercancel', onUp);
    }

    /**
     * Seeks to a position based on clientX
     */
    _seekToPosition(clientX) {
        if (!this.previewSourceId || !this.durationMs) return;

        const rect = this.previewWaveform.getBoundingClientRect();
        const percent = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
        const posMs = Math.floor(percent * this.durationMs);

        // Update visuals immediately (no IPC)
        VectorSvelte.setVoicePreview({ progress: percent * 100, time: this._formatTime(posMs / 1000) });

        // Throttle the actual engine seek to avoid audio glitching during drag
        this._pendingSeekMs = posMs;
        if (!this._seekThrottleTimer) {
            this._seekThrottleTimer = setTimeout(() => {
                this._seekThrottleTimer = null;
                if (this._pendingSeekMs != null) {
                    invoke('audio_seek', { id: this.previewSourceId, positionMs: this._pendingSeekMs }).catch(() => {});
                    if (this.isPlaying) {
                        this.playStartTime = performance.now();
                        this.playStartPos = this._pendingSeekMs;
                    }
                }
            }, 50);
        }
    }

    _formatTime(seconds) {
        const current = Math.floor(seconds);
        const minutes = Math.floor(current / 60);
        const secs = current % 60;
        return `${minutes}:${secs.toString().padStart(2, '0')}`;
    }

    /**
     * Smooth progress bar update using requestAnimationFrame
     */
    _updatePreviewProgress() {
        if (!this.isPlaying || !this.durationMs) return;

        const elapsed = performance.now() - this.playStartTime;
        const posMs = Math.min(this.playStartPos + elapsed, this.durationMs);
        const percent = (posMs / this.durationMs) * 100;
        VectorSvelte.setVoicePreview({ progress: percent, time: this._formatTime(posMs / 1000) });

        if (posMs >= this.durationMs) return;

        this.previewAnimationId = requestAnimationFrame(() => this._updatePreviewProgress());
    }

    /**
     * Starts the smooth progress bar animation
     */
    _startPreviewAnimation() {
        this._stopPreviewAnimation();
        this._updatePreviewProgress();
    }

    /**
     * Stops the smooth progress bar animation
     */
    _stopPreviewAnimation() {
        if (this.previewAnimationId) {
            cancelAnimationFrame(this.previewAnimationId);
            this.previewAnimationId = null;
        }
    }

    /**
     * Handles audio playback end (from engine event)
     */
    _onPreviewEnded() {
        this._stopPreviewAnimation();
        VectorSvelte.releasePlayback('voice-preview');

        this.playStartPos = 0;
        this.isPlaying = false;
        VectorSvelte.setVoicePreview({ playing: false, progress: 0, time: '0:00' });
    }

    /**
     * Returns whether recorder is in preview state
     */
    get isInPreview() {
        return this.state === RecordingState.PREVIEW;
    }

    /**
     * Returns whether recorder is actively recording
     */
    get isRecording() {
        return this.state === RecordingState.RECORDING || this.state === RecordingState.LOCKED;
    }
}

/**
 * Handles voice transcription UI and model management.
 */
class VoiceTranscriptionUI {
    constructor() {
        this.isSettingUp = false;
    }

    /**
     * Ensures the selected voice model is ready for transcription.
     * Downloads the model if not already available.
     * @param {HTMLElement} transcribeBtn - The transcribe button element
     * @returns {Promise<boolean>} True if model is ready, false otherwise
     */
    /**
     * Ensures the selected voice model is ready for transcription, downloading it if
     * needed. With `showProgress` the download paints into the player that asked (the
     * store the AudioPlayer component renders); without it the download runs silently.
     * @returns {Promise<boolean>} True if model is ready
     */
    async ensureModelReady(showProgress = false) {
        if (this.isSettingUp) return false;

        const selectedModel = window.voiceSettings?.selectedModel;
        if (!selectedModel) return false;
        const model = window.voiceSettings?.models?.find(m => m.model.name === selectedModel);
        if (model?.downloaded) return true;

        this.isSettingUp = true;
        if (!showProgress) {
            try {
                await window.voiceSettings?.downloadModel(selectedModel);
                return true;
            } catch {
                return false;
            } finally {
                this.isSettingUp = false;
            }
        }

        VectorSvelte.setModelDownload({ active: true, pct: 0, text: 'Downloading model...', failed: false });
        let unlisten = null;
        try {
            unlisten = await window.__TAURI__.event.listen('whisper_download_progress', (event) => {
                const progress = event.payload.progress;
                VectorSvelte.setModelDownload({ pct: progress, text: `Downloading... ${progress}%` });
            });
            await window.voiceSettings.downloadModel(selectedModel);
            VectorSvelte.setModelDownload({ pct: 100, text: 'Ready!' });
            setTimeout(() => VectorSvelte.setModelDownload({ active: false }), 1000);
            return true;
        } catch (error) {
            VectorSvelte.setModelDownload({ text: 'Download Failed', failed: true });
            setTimeout(() => VectorSvelte.setModelDownload({ active: false, failed: false }), 3000);
            return false;
        } finally {
            if (unlisten) unlisten();
            this.isSettingUp = false;
        }
    }

    /**
     * Transcribes an audio file using the selected model.
     * @param {string} filePath - Path to the audio file
     * @returns {Promise<Object>} Transcription data with sections and metadata
     */
    async transcribeAudioFile(filePath) {
        // We don't pass the button here since this might be called directly
        if (!await this.ensureModelReady()) {
            throw new Error("Voice model setup failed");
        }

        const selectedModel = window.voiceSettings?.selectedModel || 'small';
        return await invoke('transcribe', {
            filePath: filePath,
            modelName: selectedModel,
            translate: window.voiceSettings?.autoTranslate || false
        });
    }
}

/** What the audio player component (components/chat/attachments/AudioPlayer.svelte) needs
 *  from the app: the Rust engine, the tag reader, the transcriber and a few facts. */
/**
 * AudioPlayerHelpers: the in-chat audio player and its transcription panel.
 * @typedef {Object} AudioPlayerHelpers
 * @property {(path: string) => Promise<object|null>} probe
 * @property {(path: string) => Promise<object|null>} metadata
 * @property {(path: string) => Promise<string>} load          returns the source id
 * @property {(sourceId: string) => void} play
 * @property {(sourceId: string) => void} pause
 * @property {(sourceId: string, ms: number) => void} seek
 * @property {(sourceId: string) => void} stop
 * @property {(event: string, fn: (e: object) => void) => Promise<() => void>} listen
 * @property {() => string} glowColor
 * @property {(secs: number) => string} formatTime
 * @property {(att: object) => boolean} transcriptionSupported
 * @property {(path: string) => Promise<void>} transcribe
 * @property {() => void} cancelModelDownload
 * @property {(msg: object) => boolean} autoTranscribe
 * @property {(lang: string) => string} flag
 * @property {(el: Element) => void} twemojify
 * @property {(pendingId: string) => Promise<void>} cancelUpload
 * @property {() => () => void} holdScroll  pins the conversation's distance from its bottom; call the result to re-apply it
 * @property {(chatId: string, msgId: string) => {att: object, msg: object}|null} nextVoice  the downloaded voice message right after this one
 * @property {() => string} openChat
 * @property {(el: Element) => void} reveal  eases the conversation until `el` is in view
 * @property {(src: string) => void} viewImage
 */
const AUDIO_PLAYER_HELPERS = {
    probe: (path) => invoke('audio_probe', { path }),
    metadata: (path) => invoke('get_audio_metadata', { path }),
    load: (path) => invoke('audio_load', { path }),
    play: (id) => invoke('audio_play', { id }),
    pause: (id) => invoke('audio_pause', { id }),
    seek: (id, positionMs) => invoke('audio_seek', { id, positionMs }),
    stop: (id) => invoke('audio_stop', { id }),
    listen: (event, fn) => window.__TAURI__.event.listen(event, fn),
    glowColor: () => getComputedStyle(document.documentElement).getPropertyValue('--voice-frequency-glow').trim(),
    formatTime: (seconds) => {
        const mins = Math.floor(seconds / 60);
        const secs = Math.floor(seconds % 60);
        return `${mins}:${secs.toString().padStart(2, '0')}`;
    },
    // Voice messages (no file name) in a format Whisper reads, on a platform that has it.
    transcriptionSupported: (att) => !att.name && !!platformFeatures.transcription && ['wav', 'mp3', 'flac'].includes(att.extension),
    transcribe: async (path) => {
        if (!await window.cTranscriber.ensureModelReady(true)) throw new Error('Voice model setup failed');
        return window.cTranscriber.transcribeAudioFile(path);
    },
    cancelModelDownload: () => invoke('cancel_whisper_download'),
    // A received message under a minute old, with the chosen model already on disk.
    autoTranscribe: (msg) => {
        if (!window.voiceSettings?.autoTranscribe || msg.mine || msg.at <= Date.now() - 60_000) return false;
        const selected = window.voiceSettings.selectedModel || 'small';
        return !!window.voiceSettings.models?.find(m => m.model.name === selected)?.downloaded;
    },
    flag: (lang) => isoToFlagEmoji(lang),
    twemojify: (el) => twemojify(el),
    cancelUpload: (pendingId) => invoke('cancel_upload', { pendingId }),
    // Absolute, not a running sum of per-frame nudges: each nudge is rounded, and an
    // open and a close rounded apart leave the conversation a little higher every time.
    holdScroll: () => {
        const fromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop;
        return () => { domChatMessages.scrollTop = domChatMessages.scrollHeight - fromBottom; };
    },
    nextVoice: (chatId, msgId) => {
        const msgs = _dmsgListHelpers.messages(chatId);
        const i = msgs.findIndex(m => m.id === msgId);
        const msg = i >= 0 ? msgs[i + 1] : null;
        const att = msg?.attachments?.find(a => !a.name && a.downloaded && a.path && _dmsgMediaHelpers.isAudio(a.extension));
        return att ? { att, msg } : null;
    },
    openChat: () => strOpenChat,
    // The least travel that shows it, top first when it is taller than the view.
    reveal: (el) => {
        const view = domChatMessages.getBoundingClientRect(), r = el.getBoundingClientRect(), pad = 16;
        const delta = r.bottom + pad > view.bottom ? Math.min(r.bottom + pad - view.bottom, r.top - pad - view.top)
            : r.top - pad < view.top ? r.top - pad - view.top : 0;
        if (delta) domChatMessages.scrollBy({ top: delta, behavior: 'smooth' });
    },
    viewImage: (src) => openImageViewer(src),
};

/**
 * MediaPopoutHelpers: the floating player that carries media out of its chat.
 * @typedef {Object} MediaPopoutHelpers
 * @property {AudioPlayerHelpers} audio
 * @property {(chatId: string) => string} chatName
 * @property {(chatId: string) => {community: string, channel: string, icon: string}|null} channelOf  a community channel's place, null elsewhere
 * @property {(msg: object, chatId: string) => {npub: string, name: string, avatar: string}} who
 * @property {(chatId: string, msgId: string) => void} openAt
 */
const MEDIA_POPOUT_HELPERS = {
    audio: AUDIO_PLAYER_HELPERS,
    chatName: (chatId) => {
        if (chatId === strPubkey) return 'Notes';
        const chat = arrChats.find(c => c.id === chatId);
        return chat && chatIsGroup(chat) ? communityChatTitle(chat) || '' : getName(chatId);
    },
    channelOf: (chatId) => {
        const chat = arrChats.find(c => c.id === chatId);
        if (!chat || !communityIdOfChat(chat)) return null;
        const cf = chat.metadata?.custom_fields || {};
        const cid = communityIdOfChat(chat);
        const withIcon = chat.metadata?.avatar_cached ? chat : arrChats.find(c => c.metadata?.avatar_cached && communityIdOfChat(c) === cid);
        const icon = withIcon ? convertFileSrc(withIcon.metadata.avatar_cached) : 'icons/group-placeholder.svg';
        return { community: cf.name || '', channel: cf.channel_name || '', icon };
    },
    who: (msg, chatId) => {
        const npub = msg.mine ? strPubkey : (msg.npub || chatId);
        const p = getProfile(npub);
        return { npub, name: getName(p || npub), avatar: getProfileAvatarSrc(p) || 'icons/user-placeholder.svg' };
    },
    openAt: async (chatId, msgId) => {
        await openChat(chatId);
        jumpToMessage(msgId);
    },
};

document.addEventListener('DOMContentLoaded', () => {
    // A reload (an account swap) never unmounts a player, so its sources would play on.
    invoke('audio_stop_all').catch(() => {});
    VectorSvelte.setScreen('mediaPopout', { h: MEDIA_POPOUT_HELPERS });
});
