/**
 * Voice recording functionality with start/stop controls.
 */
class VoiceRecorder {
    /**
     * @param {HTMLElement} button - The recording button element
     */
    constructor(button) {
        this.button = button;
        this.isRecording = false;
    }

    /**
     * Toggles recording state between start and stop.
     * @returns {Promise<Uint8Array|null>} Audio data when stopping, null when starting
     */
    async toggle() {
        return this.isRecording ? this.stop() : this.start();
    }

    /**
     * Starts audio recording.
     * @returns {Promise<void>}
     */
    async start() {
        try {
            await invoke('start_recording');
            this.isRecording = true;
            this.button.innerHTML = '<span class="icon icon-mic-off"></span>';
            return true;
        } catch (err) {
            console.error('Recording start failed:', err);
            await popupConfirm('Recording Error', err, true);
            this.isRecording = false;
            return false;
        }
    }

    /**
     * Stops audio recording and returns the recorded data.
     * @returns {Promise<Uint8Array|null>} The recorded audio data
     */
    async stop() {
        try {
            const wavData = await invoke('stop_recording');
            this.isRecording = false;
            this.button.innerHTML = '<span class="icon icon-mic-on"></span>';
            return new Uint8Array(wavData);
        } catch (err) {
            console.error('Recording stop failed:', err);
            return null;
        }
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
    audPreview.src = assetUrl;
    audPreview.style.display = 'none';
    audPreview.addEventListener('loadedmetadata', () => {
        updateDuration();
        softChatScroll();
    });

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
