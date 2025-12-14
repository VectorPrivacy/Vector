/**
 * Voice recording functionality with Mobile-first UX:
 * - Hold to record (200ms threshold)
 * - Drag up to lock recording
 * - Drag left to cancel
 * - Preview before sending
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
     * @param {HTMLElement} sendButton - The send button element (optional, will be found if not provided)
     */
    constructor(button, inputContainer, sendButton = null) {
        this.button = button;
        this.inputContainer = inputContainer;
        this.sendButton = sendButton || document.getElementById('chat-input-send');
        this.state = RecordingState.IDLE;
        this.holdTimer = null;
        this.startPosition = { x: 0, y: 0 };
        this.currentPosition = { x: 0, y: 0 };
        this.recordingStartTime = null;
        this.timerInterval = null;
        this.audioData = null;
        this.isStoppingRecording = false; // Lock to prevent double stop_recording calls
        this.dragAxis = null; // 'x' for cancel, 'y' for lock - locked once determined
        this.lastMoveAxis = null; // Track previous axis for smooth transitions
        this.tooltipTimeout = null; // Timeout for hiding the "Hold to record" tooltip
        
        // Callbacks
        this.onSend = null;
        this.onCancel = null;
        this.onStateChange = null;
        
        this._createUI();
        this._bindEvents();
    }

    /**
     * Creates the recording UI elements inside the chat input container
     */
    _createUI() {
        // Get references to existing elements we'll hide during recording
        this.fileButton = this.inputContainer.querySelector('#chat-input-file');
        this.textInput = this.inputContainer.querySelector('#chat-input');
        this.emojiButton = this.inputContainer.querySelector('#chat-input-emoji');
        
        // Recording UI container (replaces textarea area during recording)
        this.recordingUI = document.createElement('div');
        this.recordingUI.className = 'voice-recording-ui';
        this.recordingUI.innerHTML = `
            <div class="voice-recorder-status">
                <span class="recording-dot"></span>
                <span class="recording-text">Recording...</span>
            </div>
            <div class="voice-recorder-slide-hint">
                <div class="slide-icon-container">
                    <span class="icon icon-chevron-double-left"></span>
                </div>
                <span class="slide-text">Slide to cancel</span>
            </div>
            <div class="voice-recorder-timer">0:00</div>
        `;
        
        // Lock indicator (positioned above the red circle)
        this.lockZone = document.createElement('div');
        this.lockZone.className = 'voice-recorder-lock-zone';
        this.lockZone.innerHTML = `
            <div class="lock-indicator">
                <span class="icon icon-locked"></span>
            </div>
            <div class="lock-arrow">
                <div class="arrow-icon-container">
                    <span class="icon icon-chevron-double-left"></span>
                </div>
            </div>
        `;
        
        // Preview UI container (replaces textarea area during preview)
        this.previewUI = document.createElement('div');
        this.previewUI.className = 'voice-preview-ui';
        this.previewUI.innerHTML = `
            <button class="voice-preview-delete"><span class="icon icon-trash"></span></button>
            <div class="voice-preview-center">
                <button class="voice-preview-play"><span class="icon icon-play"></span></button>
                <div class="voice-preview-waveform">
                    <div class="voice-preview-progress"></div>
                    <div class="voice-preview-handle"></div>
                </div>
                <span class="voice-preview-time">0:00</span>
            </div>
        `;
        
        // Red circle/stop button (overlays mic button during recording)
        this.redCircle = document.createElement('div');
        this.redCircle.className = 'voice-recorder-red-circle';
        
        // Tooltip for quick tap hint
        this.tooltip = document.createElement('div');
        this.tooltip.className = 'voice-recorder-tooltip';
        this.tooltip.textContent = 'Hold to record';
        
        // Insert recording UI before the textarea (so it appears in the middle)
        this.textInput.parentNode.insertBefore(this.recordingUI, this.textInput);
        this.textInput.parentNode.insertBefore(this.previewUI, this.textInput);
        
        // Insert red circle right after the mic button (as a sibling)
        this.button.parentNode.insertBefore(this.redCircle, this.button.nextSibling);
        
        // Insert tooltip into the chat box for positioning above mic button
        this.inputContainer.parentNode.appendChild(this.tooltip);
        
        // Insert lock zone into the chat box (parent of input container) for absolute positioning
        this.inputContainer.parentNode.appendChild(this.lockZone);
        
        // Cache UI elements
        this.slideHint = this.recordingUI.querySelector('.voice-recorder-slide-hint');
        this.timerDisplay = this.recordingUI.querySelector('.voice-recorder-timer');
        this.recordingStatus = this.recordingUI.querySelector('.voice-recorder-status');
        this.recordingText = this.recordingUI.querySelector('.recording-text');
        this.lockIndicator = this.lockZone.querySelector('.lock-indicator');
        this.lockArrow = this.lockZone.querySelector('.lock-arrow');
        
        this.previewDeleteBtn = this.previewUI.querySelector('.voice-preview-delete');
        this.previewPlayBtn = this.previewUI.querySelector('.voice-preview-play');
        this.previewWaveform = this.previewUI.querySelector('.voice-preview-waveform');
        this.previewProgress = this.previewUI.querySelector('.voice-preview-progress');
        this.previewHandle = this.previewUI.querySelector('.voice-preview-handle');
        this.previewTime = this.previewUI.querySelector('.voice-preview-time');
        
        // Audio element for preview
        this.audioElement = new Audio();
        this.isPlaying = false;
    }

    /**
     * Binds all event listeners
     */
    _bindEvents() {
        // Pointer events for cross-platform support
        this.button.addEventListener('pointerdown', this._onPointerDown.bind(this));
        document.addEventListener('pointermove', this._onPointerMove.bind(this));
        document.addEventListener('pointerup', this._onPointerUp.bind(this));
        document.addEventListener('pointercancel', this._onPointerUp.bind(this));
        
        // Red circle click (for locked state)
        this.redCircle.addEventListener('click', this._onRedCircleClick.bind(this));
        
        // Preview controls
        this.previewDeleteBtn.addEventListener('click', this._onPreviewDelete.bind(this));
        this.previewPlayBtn.addEventListener('click', this._onPreviewPlayPause.bind(this));
        
        // Waveform seeking - support both click and drag
        this.previewWaveform.addEventListener('pointerdown', this._onWaveformPointerDown.bind(this));
        
        this.audioElement.addEventListener('ended', this._onAudioEnded.bind(this));
    }

    /**
     * Handles play/pause button click in preview
     */
    async _onPreviewPlayPause() {
        if (this.isPlaying) {
            this.audioElement.pause();
            this.isPlaying = false;
            this._stopProgressAnimation();
            this.previewPlayBtn.innerHTML = '<span class="icon icon-play"></span>';
        } else {
            this.previewPlayBtn.innerHTML = '<span class="icon icon-pause"></span>';
            
            try {
                this.playbackStartPosition = this.audioElement.currentTime;
                
                // Wait for audio to actually start playing before starting the timer
                const playingPromise = new Promise(resolve => {
                    const onPlaying = () => {
                        this.audioElement.removeEventListener('playing', onPlaying);
                        resolve();
                    };
                    this.audioElement.addEventListener('playing', onPlaying);
                });
                
                await this.audioElement.play();
                await playingPromise;
                
                // Now the audio is actually playing, start the timer
                this.playbackStartTime = performance.now();
                this.isPlaying = true;
                this._startProgressAnimation();
            } catch (err) {
                console.error('Playback failed:', err);
                this.previewPlayBtn.innerHTML = '<span class="icon icon-play"></span>';
            }
        }
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
            // Don't add transition - user is still actively dragging
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
        
        this.tooltip.classList.add('visible');
        
        this.tooltipTimeout = setTimeout(() => {
            this.tooltip.classList.remove('visible');
            this.tooltipTimeout = null;
        }, 2000);
    }
    
    /**
     * Resets drag state (axis lock and red circle position)
     */
    _resetDragState() {
        this.dragAxis = null;
        this.lastMoveAxis = null;
        if (this.redCircle) {
            // Add returning class for smooth animation back to origin
            this.redCircle.classList.add('returning');
            this.redCircle.style.transform = '';
            
            // Remove the returning class after animation completes
            setTimeout(() => {
                this.redCircle.classList.remove('returning');
            }, 200);
        }
    }

    /**
     * Starts the actual recording
     */
    async _startRecording() {
        try {
            // Reset status text for new recording
            this.recordingText.textContent = 'Recording...';
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
     * Stops recording and shows preview
     */
    async _stopRecording() {
        // Prevent double calls while stopping
        if (this.isStoppingRecording) return;
        this.isStoppingRecording = true;
        
        // Update status text to show we're finishing
        this.recordingText.textContent = 'Finishing...';
        
        try {
            const wavData = await invoke('stop_recording');
            this._stopTimer();
            this.audioData = new Uint8Array(wavData);
            
            // Create blob URL for preview
            const blob = new Blob([this.audioData], { type: 'audio/wav' });
            const blobUrl = URL.createObjectURL(blob);
            
            // Wait for audio to be fully loaded before showing preview
            this.audioElement.src = blobUrl;
            this.audioElement.preload = 'auto';
            
            // Wait for the audio to be ready
            await new Promise((resolve) => {
                const onCanPlay = () => {
                    this.audioElement.removeEventListener('canplaythrough', onCanPlay);
                    resolve();
                };
                this.audioElement.addEventListener('canplaythrough', onCanPlay);
                // Also resolve if already ready
                if (this.audioElement.readyState >= 4) {
                    resolve();
                }
            });
            
            this._setState(RecordingState.PREVIEW);
        } catch (err) {
            console.error('Recording stop failed:', err);
            this._setState(RecordingState.IDLE);
        } finally {
            this.isStoppingRecording = false;
        }
    }

    /**
     * Locks the recording (allows releasing finger)
     */
    _lockRecording() {
        // Reset drag state (red circle returns to origin)
        this._resetDragState();
        
        // Fade out the lock zone before transitioning to locked state
        this.lockZone.classList.add('fading-out');
        
        // Wait for fade out transition, then update state
        setTimeout(() => {
            this.lockZone.classList.remove('fading-out', 'active');
            this._setState(RecordingState.LOCKED);
        }, 200); // Match the CSS transition duration
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
                await invoke('stop_recording');
            } catch (err) {
                console.error('Recording cancel failed:', err);
            } finally {
                this.isStoppingRecording = false;
            }
        }
        
        clearTimeout(this.holdTimer);
        this._stopTimer();
        this.audioData = null;
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
    _onPreviewDelete() {
        this.audioElement.pause();
        this.audioElement.src = '';
        this.audioData = null;
        this._setState(RecordingState.IDLE);
        this._animateChatInputFadeIn();
        if (this.onCancel) this.onCancel();
    }

    /**
     * Sends the recorded audio
     */
    send() {
        if (this.state !== RecordingState.PREVIEW || !this.audioData) return null;
        
        const data = this.audioData;
        this.audioElement.pause();
        this.audioElement.src = '';
        this.audioData = null;
        this._setState(RecordingState.IDLE);
        this._animateChatInputFadeIn();
        
        return data;
    }

    /**
     * Animates the chat input elements with a fade-in effect
     */
    _animateChatInputFadeIn() {
        const elements = [this.fileButton, this.textInput, this.emojiButton, this.button];
        
        elements.forEach(el => {
            if (el) {
                el.classList.add('chat-input-fade-in');
                el.addEventListener('animationend', () => {
                    el.classList.remove('chat-input-fade-in');
                }, { once: true });
            }
        });
    }

    /**
     * Updates the state and UI
     */
    _setState(newState) {
        const oldState = this.state;
        this.state = newState;
        
        // Update UI based on state
        this._updateUI();
        
        if (this.onStateChange) {
            this.onStateChange(newState, oldState);
        }
    }

    /**
     * Updates UI based on current state
     */
    _updateUI() {
        // Reset all states - show normal input elements
        this.button.classList.remove('recording', 'pending');
        this.button.style.display = '';
        this.redCircle.classList.remove('active', 'locked', 'pending');
        this.recordingUI.classList.remove('active', 'cancelling');
        this.previewUI.classList.remove('active');
        this.lockZone.classList.remove('active', 'locked', 'fading-out');
        this.lockZone.style.opacity = '';
        
        // Show normal input elements
        if (this.fileButton) this.fileButton.style.display = '';
        if (this.textInput) this.textInput.style.display = '';
        if (this.emojiButton) this.emojiButton.style.display = '';
        
        // Hide send button by default (will be shown in preview state)
        if (this.sendButton) {
            this.sendButton.style.display = 'none';
            this.sendButton.classList.remove('active', 'voice-preview-send');
        }
        
        // Reset drag feedback
        if (this.slideHint) {
            this.slideHint.style.display = '';
            this.slideHint.style.opacity = '1';
            this.slideHint.style.transform = '';
            const iconEl = this.slideHint.querySelector('.icon');
            if (iconEl) {
                iconEl.style.backgroundColor = '';
            }
        }
        if (this.lockIndicator) this.lockIndicator.style.transform = '';
        if (this.lockArrow) this.lockArrow.style.opacity = '';
        
        // Reset timer display
        if (this.timerDisplay) {
            this.timerDisplay.textContent = '0:00';
            this.timerDisplay.style.opacity = '';
        }
        
        // Reset recording status opacity
        if (this.recordingStatus) {
            this.recordingStatus.style.opacity = '';
        }
        
        // Reset preview progress/handle
        if (this.previewProgress) this.previewProgress.style.width = '0%';
        if (this.previewHandle) this.previewHandle.style.left = '0';
        if (this.previewTime) this.previewTime.textContent = '0:00';
        
        switch (this.state) {
            case RecordingState.IDLE:
                this.button.innerHTML = '<span class="icon icon-mic-on"></span>';
                // Reset play state
                this.isPlaying = false;
                if (this.previewPlayBtn) {
                    this.previewPlayBtn.innerHTML = '<span class="icon icon-play"></span>';
                }
                break;
                
            case RecordingState.PENDING:
                // Fade out mic button, fade in red circle
                this.button.classList.add('pending');
                this.redCircle.classList.add('pending');
                break;
                
            case RecordingState.RECORDING:
                // Hide normal input elements
                if (this.fileButton) this.fileButton.style.display = 'none';
                if (this.textInput) this.textInput.style.display = 'none';
                if (this.emojiButton) this.emojiButton.style.display = 'none';
                
                // Show recording UI and red circle (hide mic button)
                this.recordingUI.classList.add('active');
                this.redCircle.classList.add('active');
                this.lockZone.classList.add('active');
                this.button.classList.add('recording');
                
                // Force restart the chevron animation (fixes animation stopping after cancel)
                if (this.slideHint) {
                    const iconEl = this.slideHint.querySelector('.icon');
                    if (iconEl) {
                        iconEl.style.animation = 'none';
                        // Trigger reflow to ensure the animation reset takes effect
                        void iconEl.offsetHeight;
                        iconEl.style.animation = '';
                    }
                }
                break;
                
            case RecordingState.LOCKED:
                // Hide normal input elements
                if (this.fileButton) this.fileButton.style.display = 'none';
                if (this.textInput) this.textInput.style.display = 'none';
                if (this.emojiButton) this.emojiButton.style.display = 'none';
                
                // Show recording UI with locked state (hide slide hint)
                this.recordingUI.classList.add('active');
                this.redCircle.classList.add('active', 'locked');
                this.lockZone.classList.add('active', 'locked');
                this.button.style.display = 'none';
                // Hide slide to cancel when locked
                if (this.slideHint) this.slideHint.style.display = 'none';
                break;
                
            case RecordingState.PREVIEW:
                // Hide normal input elements and mic button
                if (this.fileButton) this.fileButton.style.display = 'none';
                if (this.textInput) this.textInput.style.display = 'none';
                if (this.emojiButton) this.emojiButton.style.display = 'none';
                this.button.style.display = 'none';
                
                // Show preview UI and send button
                this.previewUI.classList.add('active');
                if (this.sendButton) {
                    this.sendButton.style.display = '';
                    this.sendButton.classList.add('active', 'voice-preview-send');
                }
                break;
                
            case RecordingState.CANCELLED:
                this.recordingUI.classList.add('cancelling');
                break;
        }
    }

    /**
     * Updates drag visual feedback
     */
    _updateDragFeedback(deltaX, deltaY) {
        // Clamp values to prevent negative movement
        const clampedDeltaX = Math.max(0, deltaX);
        const clampedDeltaY = Math.max(0, deltaY);
        
        // Move red circle along the dominant axis (rail system)
        // If axis is locked, use that; otherwise use whichever axis has more movement
        if (this.redCircle) {
            // Determine current dominant axis based on movement
            let currentDominantAxis = null;
            if (clampedDeltaX > 0 || clampedDeltaY > 0) {
                currentDominantAxis = clampedDeltaX >= clampedDeltaY ? 'x' : 'y';
            }
            
            // Use locked axis if set, otherwise use current dominant
            const moveAxis = this.dragAxis || currentDominantAxis;
            
            // Track axis for other feedback (timer, slide hint, etc.)
            this.lastMoveAxis = moveAxis;
            
            if (moveAxis === 'x' && clampedDeltaX > 0) {
                // Move left along X axis only
                this.redCircle.style.transform = `translateX(${-clampedDeltaX}px) scale(1)`;
            } else if (moveAxis === 'y' && clampedDeltaY > 0) {
                // Move up along Y axis only
                this.redCircle.style.transform = `translateY(${-clampedDeltaY}px) scale(1)`;
            } else {
                // At origin, reset transform
                this.redCircle.style.transform = 'scale(1)';
            }
        }
        
        // Cancel feedback - only show when on X axis
        const cancelProgress = Math.min(clampedDeltaX / CANCEL_DRAG_THRESHOLD, 1);
        
        // Timer and recording status fade out - fades from visible to invisible during X drag
        // Timer fades faster (0-20px), recording status fades slower (0-50px)
        // Use the current move axis (locked or dominant) for immediate feedback
        const effectiveAxisForFade = this.dragAxis || this.lastMoveAxis;
        if (effectiveAxisForFade === 'x' && clampedDeltaX > 0) {
            // Timer fades completely over 20px
            const timerFadeProgress = Math.min(clampedDeltaX / 20, 1);
            // Recording status fades slower - over 50px (half the cancel threshold)
            const statusFadeProgress = Math.min(clampedDeltaX / 50, 1);
            
            if (this.timerDisplay) {
                this.timerDisplay.style.opacity = 1 - timerFadeProgress;
            }
            if (this.recordingStatus) {
                this.recordingStatus.style.opacity = 1 - statusFadeProgress;
            }
        } else {
            if (this.timerDisplay) {
                this.timerDisplay.style.opacity = '1';
            }
            if (this.recordingStatus) {
                this.recordingStatus.style.opacity = '1';
            }
        }
        
        // Slide hint follows cursor immediately when dragging left
        if (this.slideHint) {
            const effectiveAxis = this.dragAxis || this.lastMoveAxis;
            if (effectiveAxis === 'x' && clampedDeltaX > 0) {
                // Move the slide hint left and fade to red as user drags
                this.slideHint.style.transform = `translateX(${-clampedDeltaX * 0.5}px)`;
                this.slideHint.style.opacity = 1 - (cancelProgress * 0.5);
                // Change color towards red
                const iconEl = this.slideHint.querySelector('.icon');
                if (iconEl) {
                    iconEl.style.backgroundColor = cancelProgress > 0.5 ? '#ff4444' : '';
                }
            } else {
                // Reset slide hint if not on X axis
                this.slideHint.style.transform = '';
                this.slideHint.style.opacity = '1';
                const iconEl = this.slideHint.querySelector('.icon');
                if (iconEl) iconEl.style.backgroundColor = '';
            }
        }
        
        // Lock zone feedback - only show when on Y axis
        if (this.lockZone) {
            if (this.dragAxis !== 'y' || clampedDeltaY < LOCK_SHOW_THRESHOLD) {
                // Keep lock zone hidden
                this.lockZone.style.opacity = 0;
                if (this.lockIndicator) this.lockIndicator.style.transform = '';
                if (this.lockArrow) this.lockArrow.style.opacity = '';
            } else {
                // Calculate progress from show threshold to lock threshold
                const visibleProgress = Math.min((clampedDeltaY - LOCK_SHOW_THRESHOLD) / (LOCK_DRAG_THRESHOLD - LOCK_SHOW_THRESHOLD), 1);
                // Gradually increase opacity from 0 to 1 as user drags up
                this.lockZone.style.opacity = visibleProgress;
                // Move lock indicator up and scale
                if (this.lockIndicator) {
                    this.lockIndicator.style.transform = `translateY(${-visibleProgress * 15}px) scale(${1 + visibleProgress * 0.2})`;
                }
                // Hide arrow as we approach lock
                if (this.lockArrow) {
                    this.lockArrow.style.opacity = 1 - visibleProgress;
                }
            }
        }
    }

    /**
     * Starts the recording timer
     */
    _startTimer() {
        this.timerInterval = setInterval(() => {
            const elapsed = Math.floor((Date.now() - this.recordingStartTime) / 1000);
            const minutes = Math.floor(elapsed / 60);
            const seconds = elapsed % 60;
            this.timerDisplay.textContent = `${minutes}:${seconds.toString().padStart(2, '0')}`;
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
     * Handles seek in preview waveform
     */
    /**
     * Handles pointer down on waveform for seeking
     */
    _onWaveformPointerDown(e) {
        e.preventDefault();
        this.isSeeking = true;
        
        // Capture pointer for reliable mobile drag support
        this.previewWaveform.setPointerCapture(e.pointerId);
        
        // Seek immediately on pointer down
        this._seekToPosition(e.clientX);
        
        // Bind move and up handlers to document for reliable mobile tracking
        const onMove = (moveEvent) => {
            this._seekToPosition(moveEvent.clientX);
        };
        
        const onUp = (upEvent) => {
            this.isSeeking = false;
            try {
                this.previewWaveform.releasePointerCapture(upEvent.pointerId);
            } catch (err) {
                // Pointer may already be released
            }
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
        const rect = this.previewWaveform.getBoundingClientRect();
        const percent = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
        
        if (this.audioElement.duration) {
            const newTime = percent * this.audioElement.duration;
            this.audioElement.currentTime = newTime;
            
            // Reset playback tracking for smooth animation after seek
            if (this.isPlaying) {
                this.playbackStartTime = performance.now();
                this.playbackStartPosition = newTime;
            }
            
            // Update progress bar and time display immediately
            this.previewProgress.style.width = `${percent * 100}%`;
            this.previewHandle.style.left = `calc(${percent * 100}% - 7px)`;
            this._updateTimeDisplay(newTime);
        }
    }

    /**
     * Updates the time display
     */
    _updateTimeDisplay(currentTime) {
        const current = Math.floor(currentTime !== undefined ? currentTime : this.audioElement.currentTime);
        const minutes = Math.floor(current / 60);
        const seconds = current % 60;
        this.previewTime.textContent = `${minutes}:${seconds.toString().padStart(2, '0')}`;
    }

    /**
     * Smooth progress bar update using requestAnimationFrame
     * Uses calculated time based on performance.now() for smooth animation
     */
    _updateProgressBar() {
        if (!this.isPlaying || !this.audioElement.duration) return;
        
        // Calculate time based on elapsed time since playback started
        // This avoids the jitter from audioElement.currentTime
        const elapsed = (performance.now() - this.playbackStartTime) / 1000;
        const calculatedTime = Math.min(this.playbackStartPosition + elapsed, this.audioElement.duration);
        
        const percent = (calculatedTime / this.audioElement.duration) * 100;
        this.previewProgress.style.width = `${percent}%`;
        this.previewHandle.style.left = `calc(${percent}% - 7px)`;
        
        // Update time display with calculated time
        this._updateTimeDisplay(calculatedTime);
        
        // Check if we've reached the end
        if (calculatedTime >= this.audioElement.duration) {
            return; // Let the 'ended' event handle cleanup
        }
        
        this.progressAnimationFrame = requestAnimationFrame(() => this._updateProgressBar());
    }

    /**
     * Starts the smooth progress bar animation
     */
    _startProgressAnimation() {
        if (this.progressAnimationFrame) {
            cancelAnimationFrame(this.progressAnimationFrame);
        }
        this._updateProgressBar();
    }

    /**
     * Stops the smooth progress bar animation
     */
    _stopProgressAnimation() {
        if (this.progressAnimationFrame) {
            cancelAnimationFrame(this.progressAnimationFrame);
            this.progressAnimationFrame = null;
        }
    }

    /**
     * Handles audio playback end
     */
    _onAudioEnded() {
        this._stopProgressAnimation();
        
        // Reset audio position to beginning
        this.audioElement.currentTime = 0;
        this.playbackStartPosition = 0;
        
        // Reset visual progress
        this.previewProgress.style.width = '0%';
        this.previewHandle.style.left = '0';
        this._updateTimeDisplay(0);
        
        this.isPlaying = false;
        this.previewPlayBtn.innerHTML = '<span class="icon icon-play"></span>';
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
    async ensureModelReady(transcribeBtn) {
        if (this.isSettingUp) return false;
        
        const selectedModel = window.voiceSettings?.selectedModel || 'small';
        const model = window.voiceSettings?.models?.find(m => m.model.name === selectedModel);
        if (model?.downloaded) return true;
        
        this.isSettingUp = true;

        // Store original button contents
        const originalHTML = transcribeBtn.innerHTML;
        const originalClasses = transcribeBtn.className;
        
        // Hide audio player UI elements but keep transcribe button visible
        const audioContainer = transcribeBtn.closest('.audio-message-container');
        const playBtn = audioContainer?.querySelector('.audio-play-btn');
        const waveform = audioContainer?.querySelector('.audio-waveform');
        const timeDisplay = audioContainer?.querySelector('.audio-time-display');
        
        if (playBtn) playBtn.style.display = 'none';
        if (waveform) waveform.style.display = 'none';
        if (timeDisplay) timeDisplay.style.display = 'none';
        
        // Create progress elements inside the transcribe button
        const progressContainer = document.createElement('div');
        progressContainer.classList.add('transcribe-progress-container');
        
        const progressBar = document.createElement('div');
        progressBar.classList.add('transcribe-progress-bar');
        
        const progressFill = document.createElement('div');
        progressFill.classList.add('transcribe-progress-fill');
        
        const progressText = document.createElement('div');
        progressText.classList.add('transcribe-progress-text');
        progressText.textContent = 'Downloading model...';
        
        progressBar.appendChild(progressFill);
        progressContainer.appendChild(progressText);
        progressContainer.appendChild(progressBar);
        
        // Replace button contents with progress indicator
        transcribeBtn.style.marginLeft = 'auto';
        transcribeBtn.style.marginRight = 'auto';
        transcribeBtn.innerHTML = '';
        transcribeBtn.appendChild(progressContainer);
        transcribeBtn.classList.add('downloading');
        
        try {
            const unlisten = await window.__TAURI__.event.listen(
                'whisper_download_progress', 
                (event) => {
                    const progress = event.payload.progress;
                    progressFill.style.width = `${progress}%`;
                    progressText.textContent = `Downloading... ${progress}%`;
                }
            );

            await window.voiceSettings.downloadModel(selectedModel);
            unlisten();
            
            progressText.textContent = 'Ready!';
            setTimeout(() => {
                transcribeBtn.classList.remove('downloading');
            }, 1000);

        // Restore original button state
        transcribeBtn.innerHTML = originalHTML;
        transcribeBtn.className = originalClasses;
        transcribeBtn.style.marginLeft = '';
        transcribeBtn.style.marginRight = '';
        
        // Show audio player UI again
        if (playBtn) playBtn.style.display = '';
        if (waveform) waveform.style.display = '';
        if (timeDisplay) timeDisplay.style.display = '';
            
            return true;
        } catch (error) {
            progressText.textContent = `Download failed`;
            progressFill.style.background = '#ff5e5e';
            setTimeout(() => {
                transcribeBtn.classList.remove('downloading');
                // Restore original button state
                transcribeBtn.innerHTML = originalHTML;
                transcribeBtn.className = originalClasses;
                transcribeBtn.style.marginLeft = '';
                transcribeBtn.style.marginRight = '';
                // Show audio player UI again
                if (playBtn) playBtn.style.display = '';
                if (waveform) waveform.style.display = '';
                if (timeDisplay) timeDisplay.style.display = '';
            }, 3000);
            return false;
        } finally {
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

/**
 * Creates a clickable transcription section element.
 * @param {Object} section - Section data with text and timestamp
 * @param {number} index - Section index
 * @param {HTMLAudioElement} audioElement - Audio player for seeking
 * @returns {HTMLSpanElement} The transcription section element
 */
function createTranscriptionSection(section, index, audioElement) {
    const sectionSpan = document.createElement('span');
    sectionSpan.classList.add('transcription-section');
    sectionSpan.setAttribute('data-timestamp', section.at);
    sectionSpan.setAttribute('data-index', index);
    sectionSpan.textContent = section.text;
    sectionSpan.style.cursor = 'pointer';
    
    // Add click functionality to seek audio
    sectionSpan.addEventListener('click', () => {
        const timestamp = parseFloat(section.at) / 1000; // Convert milliseconds to seconds
        audioElement.currentTime = timestamp;
    });
    
    return sectionSpan;
}

/**
 * Highlights the current transcription section based on audio playback time.
 * @param {HTMLElement} transcriptionContainer - Container with transcription sections
 * @param {number} currentTime - Current audio time in milliseconds
 */
function highlightCurrentSection(transcriptionContainer, currentTime) {
    const sections = transcriptionContainer.querySelectorAll('.transcription-section');
    
    // Clear all highlights first
    sections.forEach(section => {
        section.style.backgroundColor = '';
        section.style.color = '';
    });
    
    // Find and highlight the active section
    for (let i = 0; i < sections.length; i++) {
        const sectionTime = parseInt(sections[i].getAttribute('data-timestamp'));
        const nextSectionTime = i < sections.length - 1 ? 
            parseInt(sections[i + 1].getAttribute('data-timestamp')) : Infinity;
        
        if (currentTime >= sectionTime && currentTime < nextSectionTime) {
            sections[i].style.backgroundColor = 'var(--voice-highlight-bg)';
            sections[i].style.color = 'var(--voice-highlight-text)';
            break;
        }
    }
}

/**
 * Clears all highlighting from transcription sections.
 * @param {HTMLElement} transcriptionContainer - Container with transcription sections
 */
function clearHighlighting(transcriptionContainer) {
    const sections = transcriptionContainer.querySelectorAll('.transcription-section');
    sections.forEach(section => {
        section.style.backgroundColor = '';
        section.style.color = '';
    });
}

/**
 * Sets up audio time tracking for transcription highlighting.
 * @param {HTMLAudioElement} audioElement - The audio player
 * @param {HTMLElement} transcriptionContainer - Container with transcription sections
 */
function setupAudioTracking(audioElement, transcriptionContainer) {
    audioElement.addEventListener('timeupdate', () => {
        const currentTime = audioElement.currentTime * 1000; // Convert to milliseconds
        highlightCurrentSection(transcriptionContainer, currentTime);
    });
    
    audioElement.addEventListener('pause', () => {
        clearHighlighting(transcriptionContainer);
    });
    
    audioElement.addEventListener('ended', () => {
        clearHighlighting(transcriptionContainer);
    });
}

/**
 * Creates the transcription UI elements and populates them with data.
 * @param {Object} transcriptionData - The transcription data from the backend
 * @param {HTMLAudioElement} audioElement - The audio player element
 * @returns {HTMLElement} The transcription result container
 */
function createTranscriptionUI(transcriptionData, audioElement) {
    const transcriptionText = document.createElement('div');
    transcriptionText.classList.add('transcription-text');
    
    if (transcriptionData.sections?.length > 0) {
        transcriptionData.sections.forEach((section, index) => {
            const sectionSpan = createTranscriptionSection(section, index, audioElement);
            transcriptionText.appendChild(sectionSpan);
            
            // Add space between sections (except for the last one)
            if (index < transcriptionData.sections.length - 1) {
                transcriptionText.appendChild(document.createTextNode(' '));
            }
        });
        
        setupAudioTracking(audioElement, transcriptionText);
    } else {
        const noTranscription = document.createElement('span');
        noTranscription.textContent = 'No transcription available';
        transcriptionText.appendChild(noTranscription);
    }
    
    // Add language detection info if available and Auto Translation is enabled
    if (window.voiceSettings?.autoTranslate && 
        transcriptionData.lang && 
        transcriptionData.lang !== 'auto' && 
        transcriptionData.lang !== 'GB') {
        
        const langInfo = document.createElement('div');
        langInfo.style.fontSize = '0.8em';
        langInfo.style.color = 'rgba(255, 255, 255, 0.6)';
        langInfo.style.marginTop = '5px';
        
        const flagEmoji = isoToFlagEmoji(transcriptionData.lang);
        langInfo.textContent = `Original language: ${transcriptionData.lang} ${flagEmoji}`;
        twemojify(langInfo);
        transcriptionText.appendChild(langInfo);
    }
    
    return transcriptionText;
}

/**
 * Handles audio attachment rendering and transcription functionality.
 * @param {Object} cAttachment - Attachment data
 * @param {string} assetUrl - URL to the audio asset
 * @param {HTMLElement} pMessage - Message container element
 * @param {Object} msg - Message data
 */
function handleAudioAttachment(cAttachment, assetUrl, pMessage, msg) {
    const audioContainer = document.createElement('div');
    audioContainer.classList.add('audio-message-container', 'custom-audio-player');

    // Create hidden audio element for playback control
    const audPreview = document.createElement('audio');
    audPreview.crossOrigin = 'anonymous';
    audPreview.preload = 'metadata';
    audPreview.style.display = 'none';
    
    // Handle metadata loaded event
    const onMetadataLoaded = () => {
        updateDuration();
        softChatScroll();
    };
    audPreview.addEventListener('loadedmetadata', onMetadataLoaded);
    
    // Platform-specific audio creation
    if (platformFeatures.os === 'android') {
        // Android uses blob method with size limit
        createAndroidAudio(assetUrl, cAttachment, (result) => {
            if (result.blobUrl) {
                audPreview.src = result.blobUrl;
            } else if (result.errorElement) {
                // Replace the entire audio container with the error element
                audioContainer.replaceWith(result.errorElement);
            }
        });
    } else {
        // Standard audio element for other platforms
        audPreview.src = assetUrl;
    }

    // Create custom audio player
    const customPlayer = document.createElement('div');
    customPlayer.classList.add('custom-audio-player-inner');

    // Play/Pause button
    const playBtn = document.createElement('button');
    playBtn.classList.add('audio-play-btn');
    playBtn.innerHTML = '<span class="icon icon-play"></span>';
    
    // Time display
    const timeDisplay = document.createElement('div');
    timeDisplay.classList.add('audio-time-display');
    timeDisplay.innerHTML = '<span class="current-time">0:00</span> / <span class="duration">0:00</span>';

    // Transcribe Button
    const transcribeBtn = document.createElement('button');
    transcribeBtn.classList.add('audio-transcribe-btn');
    const transcribeIcon = document.createElement('span');
    transcribeIcon.classList.add('icon', 'icon-file-plus');
    transcribeBtn.appendChild(transcribeIcon);
    
    // Waveform visualization with real frequency data
    const waveform = document.createElement('div');
    waveform.classList.add('audio-waveform');
    const barCount = 32; // Number of frequency bars
    const bars = [];
    
    for (let i = 0; i < barCount; i++) {
        const bar = document.createElement('div');
        bar.classList.add('waveform-bar');
        bar.setAttribute('data-index', i);
        waveform.appendChild(bar);
        bars.push(bar);
    }
    
    // Web Audio API setup for frequency analysis
    let audioContext = null;
    let analyser = null;
    let source = null;
    let animationId = null;
    
    function initAudioAnalyser() {
        if (audioContext) return;
        
        audioContext = new AudioContext();
        analyser = audioContext.createAnalyser();
        analyser.fftSize = 256; // Increased for better frequency resolution
        analyser.smoothingTimeConstant = 0.85; // Slightly more smoothing for visual appeal
        
        source = audioContext.createMediaElementSource(audPreview);
        source.connect(analyser);
        analyser.connect(audioContext.destination);
    }
    
    function updateVisualizer() {
        if (!analyser || audPreview.paused) {
            cancelAnimationFrame(animationId);
            // Reset bars when paused but maintain playback position opacity
            const currentProgress = (audPreview.currentTime / audPreview.duration) || 0;
            bars.forEach((bar, i) => {
                bar.style.height = '20%';
                const barProgress = (i + 0.5) / barCount;
                bar.style.opacity = barProgress <= currentProgress ? '0.3' : '0.15';
            });
            return;
        }
        
        const bufferLength = analyser.frequencyBinCount;
        const dataArray = new Uint8Array(bufferLength);
        analyser.getByteFrequencyData(dataArray);
        
        // Create logarithmic scale for frequency distribution
        // Human hearing is logarithmic, so we want more bars for lower frequencies
        const minFreq = 100;  // Start from 100Hz to skip very low frequencies
        const maxFreq = 8000; // Cap at 8kHz for voice/music clarity
        const nyquist = audioContext.sampleRate / 2;
        
        // Calculate frequency bins for each bar using logarithmic scale
        const logMin = Math.log10(minFreq);
        const logMax = Math.log10(maxFreq);
        
        for (let i = 0; i < barCount; i++) {
            // Calculate frequency range for this bar
            const logFreqStart = logMin + (logMax - logMin) * (i / barCount);
            const logFreqEnd = logMin + (logMax - logMin) * ((i + 1) / barCount);
            
            const freqStart = Math.pow(10, logFreqStart);
            const freqEnd = Math.pow(10, logFreqEnd);
            
            // Convert frequencies to bin indices
            const binStart = Math.floor((freqStart / nyquist) * bufferLength);
            const binEnd = Math.ceil((freqEnd / nyquist) * bufferLength);
            
            // Average the frequency data for this range
            let sum = 0;
            let count = 0;
            for (let j = binStart; j < binEnd && j < bufferLength; j++) {
                sum += dataArray[j];
                count++;
            }
            
            const average = count > 0 ? sum / count : 0;
            
            // Apply gentler frequency-based boost
            // Lower frequencies: minimal boost, higher frequencies: moderate boost
            const freqBoost = 1 + (i / barCount) * 0.5; // Reduced from 1.5 to 0.5
            const boostedValue = average * freqBoost;
            
            // Apply dynamic range compression to prevent maxing out
            // This creates more visual variation
            const compressed = Math.tanh(boostedValue / 128) * 255; // Soft limiting
            
            // Convert to percentage with power scaling for better dynamics
            const normalizedValue = compressed / 255;
            const scaledHeight = Math.pow(normalizedValue, 1.5) * 70; // Increased power for more dynamic range
            
            // Update bar with smooth animation
            const bar = bars[i];
            bar.style.height = `${Math.max(5, Math.min(80, scaledHeight + 5))}%`;
            
            // Calculate base opacity based on frequency activity
            const baseOpacity = 0.3 + (scaledHeight / 70) * 0.7;
            
            // Apply playback position opacity adjustment
            const currentProgress = (audPreview.currentTime / audPreview.duration) || 0;
            const barProgress = (i + 0.5) / barCount; // Center of each bar
            
            // Future bars (not yet played) have reduced opacity
            const playbackOpacity = barProgress <= currentProgress ? 1 : 0.4;
            bar.style.opacity = baseOpacity * playbackOpacity;
            
            // Add glow effect for active frequencies with adjusted threshold
            if (scaledHeight > 50 && barProgress <= currentProgress) {
                const glowIntensity = (scaledHeight - 50) / 3;
                // Use theme's frequency glow color
                const glowColor = getComputedStyle(document.documentElement).getPropertyValue('--voice-frequency-glow');
                bar.style.boxShadow = `0 0 ${glowIntensity}px ${glowColor.trim()}`;
            } else {
                bar.style.boxShadow = 'none';
            }
        }
        
        animationId = requestAnimationFrame(updateVisualizer);
    }
    
    // Assemble custom player
    customPlayer.appendChild(playBtn);
    customPlayer.appendChild(waveform);
    customPlayer.appendChild(timeDisplay);
    
    audioContainer.appendChild(audPreview);
    audioContainer.appendChild(customPlayer);

    // Helper functions
    function formatTime(seconds) {
        const mins = Math.floor(seconds / 60);
        const secs = Math.floor(seconds % 60);
        return `${mins}:${secs.toString().padStart(2, '0')}`;
    }

    function updateDuration() {
        const duration = audPreview.duration;
        if (!isNaN(duration)) {
            timeDisplay.querySelector('.duration').textContent = formatTime(duration);
        }
    }

    // Play/Pause functionality with visualizer
    playBtn.addEventListener('click', async () => {
        if (audPreview.paused) {
            // Initialize audio context on first play (browser requirement)
            if (!audioContext) {
                initAudioAnalyser();
            }
            
            // Resume audio context if suspended
            if (audioContext && audioContext.state === 'suspended') {
                await audioContext.resume();
            }
            
            audPreview.play();
            playBtn.innerHTML = '<span class="icon icon-pause"></span>';
            customPlayer.classList.add('playing');
            
            // Start visualizer
            updateVisualizer();
        } else {
            audPreview.pause();
            playBtn.innerHTML = '<span class="icon icon-play"></span>';
            customPlayer.classList.remove('playing');
            
            // Stop visualizer
            if (animationId) {
                cancelAnimationFrame(animationId);
            }
        }
    });

    // Update time display
    audPreview.addEventListener('timeupdate', () => {
        const currentTime = audPreview.currentTime;
        const duration = audPreview.duration;
        
        if (!isNaN(duration)) {
            timeDisplay.querySelector('.current-time').textContent = formatTime(currentTime);
            
            // Update static bar opacity based on playback position when paused
            if (audPreview.paused) {
                updateStaticBarsOpacity(currentTime, duration);
            }
        }
    });
    
    // Function to update bar opacity when paused
    function updateStaticBarsOpacity(currentTime, duration) {
        const currentProgress = currentTime / duration;
        bars.forEach((bar, i) => {
            const barProgress = (i + 0.5) / barCount;
            const opacity = barProgress <= currentProgress ? 0.3 : 0.15;
            bar.style.opacity = opacity;
        });
    }

    // Waveform seek functionality
    let isWaveformDragging = false;
    
    function waveformSeek(e) {
        const rect = waveform.getBoundingClientRect();
        const x = Math.max(0, Math.min(e.clientX - rect.left, rect.width));
        const percentage = x / rect.width;
        
        if (!isNaN(audPreview.duration)) {
            audPreview.currentTime = percentage * audPreview.duration;
        }
    }
    
    waveform.addEventListener('mousedown', (e) => {
        isWaveformDragging = true;
        waveformSeek(e);
        document.addEventListener('mousemove', handleWaveformDrag);
        document.addEventListener('mouseup', stopWaveformDrag);
    });
    
    // Touch support for mobile
    waveform.addEventListener('touchstart', (e) => {
        isWaveformDragging = true;
        const touch = e.touches[0];
        const rect = waveform.getBoundingClientRect();
        const x = Math.max(0, Math.min(touch.clientX - rect.left, rect.width));
        const percentage = x / rect.width;
        
        if (!isNaN(audPreview.duration)) {
            audPreview.currentTime = percentage * audPreview.duration;
        }
    });
    
    waveform.addEventListener('touchmove', (e) => {
        if (isWaveformDragging) {
            e.preventDefault();
            const touch = e.touches[0];
            const rect = waveform.getBoundingClientRect();
            const x = Math.max(0, Math.min(touch.clientX - rect.left, rect.width));
            const percentage = x / rect.width;
            
            if (!isNaN(audPreview.duration)) {
                audPreview.currentTime = percentage * audPreview.duration;
            }
        }
    });
    
    waveform.addEventListener('touchend', () => {
        isWaveformDragging = false;
    });
    
    function handleWaveformDrag(e) {
        if (isWaveformDragging) {
            waveformSeek(e);
        }
    }
    
    function stopWaveformDrag() {
        isWaveformDragging = false;
        document.removeEventListener('mousemove', handleWaveformDrag);
        document.removeEventListener('mouseup', stopWaveformDrag);
    }

    // Reset on end
    audPreview.addEventListener('ended', () => {
        playBtn.innerHTML = '<span class="icon icon-play"></span>';
        customPlayer.classList.remove('playing');
        timeDisplay.querySelector('.current-time').textContent = '0:00';
        
        // Stop visualizer
        if (animationId) {
            cancelAnimationFrame(animationId);
        }
        
        // Reset bars
        bars.forEach(bar => {
            bar.style.height = '20%';
            bar.style.opacity = '0.3';
            bar.style.boxShadow = 'none';
        });
    });
    
    // Cleanup audio context when element is removed
    audioContainer.addEventListener('remove', () => {
        if (animationId) {
            cancelAnimationFrame(animationId);
        }
        if (audioContext) {
            audioContext.close();
        }
    });

    // Only add transcription UI for supported formats and platforms
    if (platformFeatures.transcription && ['wav', 'mp3', 'flac'].includes(cAttachment.extension)) {
        // Display the Transcribe button
        customPlayer.appendChild(transcribeBtn);
        
        // Add transcribe button container
        const transcribeContainer = document.createElement('div');
        transcribeContainer.classList.add('transcribe-container');
        
        // Create container for transcription result
        const transcriptionResult = document.createElement('div');
        transcriptionResult.classList.add('transcription-result', 'hidden');

        transcribeBtn.addEventListener('click', async () => {
            if (transcribeBtn.classList.contains('loading') || 
                transcribeBtn.classList.contains('downloading')) return;

            // If already transcribed, just toggle visibility
            if (transcriptionResult.textContent.trim()) {
                return transcriptionResult.classList.toggle('hidden');
            }

            // Show loading state
            transcribeBtn.classList.add('loading');
            transcribeBtn.style.cursor = 'default';
            transcribeIcon.classList.replace('icon-file-plus', 'icon-loading');
            transcribeIcon.classList.add('spin');

            try {
                // Pass the transcribe button to ensureModelReady
                if (!await window.cTranscriber.ensureModelReady(transcribeBtn)) {
                    throw new Error("Voice model setup failed");
                }

                const transcriptionData = await window.cTranscriber.transcribeAudioFile(cAttachment.path);
                
                // Restore button state
                transcribeBtn.classList.remove('loading');
                transcribeBtn.innerHTML = '';
                transcribeBtn.style.cursor = '';
                transcribeBtn.appendChild(transcribeIcon);
                transcribeIcon.classList.replace('icon-loading', 'icon-file-plus');
                transcribeIcon.classList.remove('spin');
                
                // Clear any existing content
                transcriptionResult.innerHTML = '';
                
                const transcriptionUI = createTranscriptionUI(transcriptionData, audPreview);
                transcriptionResult.appendChild(transcriptionUI);
                transcriptionResult.classList.remove('hidden');
            } catch (err) {
                console.error('Transcription error:', err);
                
                // Restore button state
                transcribeBtn.classList.remove('loading');
                transcribeBtn.innerHTML = '';
                transcribeBtn.style.cursor = '';
                transcribeBtn.appendChild(transcribeIcon);
                transcribeIcon.classList.replace('icon-loading', 'icon-file-plus');
                transcribeIcon.classList.remove('spin');
                
                transcriptionResult.innerHTML = '';
                const errorDiv = document.createElement('div');
                errorDiv.classList.add('transcription-error');
                errorDiv.textContent = `Error: ${err.message || 'Transcription failed'}`;
                transcriptionResult.appendChild(errorDiv);
                transcriptionResult.classList.remove('hidden');
            }
        });

        audioContainer.appendChild(transcribeContainer);
        audioContainer.appendChild(transcriptionResult);

        // Auto-transcribe if enabled and this is a recent received message
        if (window.voiceSettings?.autoTranscribe && 
            !msg.mine && 
            msg.at > (Date.now() - 60)) {
            
            const selectedModel = window.voiceSettings?.selectedModel || 'small';
            const currentModel = window.voiceSettings.models?.find(m => m.model.name === selectedModel);
            if (currentModel?.downloaded) {
                transcribeBtn.click();
            }
        }
    }

    pMessage.appendChild(audioContainer);
}
