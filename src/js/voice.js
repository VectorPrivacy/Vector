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
        } catch (err) {
            console.error('Recording start failed:', err);
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
     * @returns {Promise<boolean>} True if model is ready, false otherwise
     */
    async ensureModelReady() {
        if (this.isSettingUp) return false;
        
        const selectedModel = window.voiceSettings?.selectedModel || 'small';
        const model = window.voiceSettings?.models?.find(m => m.model.name === selectedModel);
        if (model?.downloaded) return true;
        
        this.isSettingUp = true;
        const progressContainer = document.getElementById('voice-progress-container');
        const progressFill = document.querySelector('.voice-progress-fill');
        const progressText = document.querySelector('.voice-progress-text');
        
        progressContainer.style.display = 'block';
        progressText.textContent = 'Downloading voice model...';
        
        try {
            // Set up progress listener
            const unlisten = await window.__TAURI__.event.listen(
                'whisper_download_progress', 
                (event) => {
                    const progress = event.payload.progress;
                    progressFill.style.width = `${progress}%`;
                    progressText.textContent = `Downloading voice model... ${progress}%`;
                }
            );

            await window.voiceSettings.downloadModel(selectedModel);
            unlisten();
            
            progressText.textContent = 'Voice model ready!';
            setTimeout(() => {
                progressContainer.style.display = 'none';
                progressFill.style.width = '0%';
            }, 1000);
            
            return true;
        } catch (error) {
            progressText.textContent = `Download failed: ${error}`;
            progressFill.style.background = '#ff5e5e';
            setTimeout(() => {
                progressContainer.style.display = 'none';
                progressFill.style.width = '0%';
                progressFill.style.background = 'linear-gradient(90deg, #59fcb3, #00d4ff)';
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

// Initialize when DOM is ready
document.addEventListener('DOMContentLoaded', async () => {
    // Initialize voice transcription with default model
    window.cTranscriber = new VoiceTranscriptionUI();
    window.voiceSettings = new VoiceSettings();

    // Load our modules chronologically
    await window.voiceSettings.loadWhisperModels();
    window.voiceSettings.initVoiceSettings();
});

/**
 * Handles audio attachment rendering and transcription functionality.
 * @param {Object} cAttachment - Attachment data
 * @param {string} assetUrl - URL to the audio asset
 * @param {HTMLElement} pMessage - Message container element
 * @param {Object} msg - Message data
 */
function handleAudioAttachment(cAttachment, assetUrl, pMessage, msg) {
    if (!['wav', 'mp3'].includes(cAttachment.extension)) return;
    
    const audioContainer = document.createElement('div');
    audioContainer.classList.add('audio-message-container');

    const audPreview = document.createElement('audio');
    audPreview.setAttribute('controlsList', 'nodownload');
    audPreview.controls = true;
    audPreview.preload = 'metadata';
    audPreview.src = assetUrl;
    audPreview.addEventListener('loadedmetadata', () => softChatScroll(), { once: true });

    // Add transcribe button container
    const transcribeContainer = document.createElement('div');
    transcribeContainer.classList.add('transcribe-container');
    
    // Add view transcription button
    const transcribeBtn = document.createElement('button');
    transcribeBtn.classList.add('btn', 'btn-transcribe');
    transcribeBtn.style.display = 'flex';

    const transcribeIcon = document.createElement('span');
    transcribeIcon.classList.add('icon', 'icon-mic-on');
    Object.assign(transcribeIcon.style, {
        position: 'relative',
        backgroundColor: 'rgba(255, 255, 255, 0.45)',
        width: '19px',
        height: '19px'
    });
    
    const transcribeText = document.createElement('span');
    transcribeText.textContent = 'Transcribe';
    Object.assign(transcribeText.style, {
        color: 'rgba(255, 255, 255, 0.45)',
        marginLeft: '5px'
    });
    
    transcribeBtn.appendChild(transcribeIcon);
    transcribeBtn.appendChild(transcribeText);
    
    // Create container for transcription result
    const transcriptionResult = document.createElement('div');
    transcriptionResult.classList.add('transcription-result', 'hidden');

    transcribeBtn.addEventListener('click', async () => {
        if (transcribeBtn.classList.contains('loading')) return;

        // If already transcribed, just toggle visibility
        if (transcriptionResult.textContent.trim()) {
            return transcriptionResult.classList.toggle('hidden');
        }

        // Show loading state
        transcribeBtn.classList.add('loading');
        transcribeText.textContent = 'Transcribing';
        transcribeBtn.style.cursor = 'default';
        transcribeIcon.classList.replace('icon-mic-on', 'icon-loading');
        transcribeIcon.classList.add('spin');

        try {
            const transcriptionData = await window.cTranscriber.transcribeAudioFile(cAttachment.path);
          
            // Remove the loading state (or, currently, the entire button)
            transcribeBtn.remove();
            
            // Clear any existing content
            transcriptionResult.innerHTML = '';
            
            const transcriptionUI = createTranscriptionUI(transcriptionData, audPreview);
            transcriptionResult.appendChild(transcriptionUI);
            transcriptionResult.classList.remove('hidden');
        } catch (err) {
            console.error('Transcription error:', err);
            
            transcriptionResult.innerHTML = '';
            const errorDiv = document.createElement('div');
            errorDiv.classList.add('transcription-error');
            errorDiv.textContent = `Error: ${err.message || 'Transcription failed'}`;
            transcriptionResult.appendChild(errorDiv);
            transcriptionResult.classList.remove('hidden');
        } finally {
            // Only clean up if button still exists (not removed on success)
            if (transcribeBtn.parentNode) {
                transcribeBtn.classList.remove('loading');
                transcribeBtn.style.cursor = '';
                transcribeIcon.classList.replace('icon-loading', 'icon-mic-on');
                transcribeIcon.classList.remove('spin');
                transcribeText.textContent = 'Transcribe';
                transcribeBtn.disabled = false;
            }
        }
    });

    transcribeContainer.appendChild(transcribeBtn);
    audioContainer.appendChild(audPreview);
    audioContainer.appendChild(transcribeContainer);
    audioContainer.appendChild(transcriptionResult);
    pMessage.appendChild(audioContainer);

    // Auto-transcribe if enabled and this is a recent received message
    if (window.voiceSettings?.autoTranscript && 
        !msg.mine && 
        msg.at > (Date.now() / 1000 - 60)) {
        
        const selectedModel = window.voiceSettings?.selectedModel || 'small';
        const currentModel = window.voiceSettings.models?.find(m => m.model.name === selectedModel);
        if (currentModel?.downloaded) {
            transcribeBtn.click();
        }
    }
}
