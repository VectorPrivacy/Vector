class VoiceRecorder {
    constructor(button) {
        this.button = button;
        this.isRecording = false;
    }

    async toggle() {
        if (this.isRecording) {
            return this.stop();
        }
        return this.start();
    }

    async start() {
        try {
            await invoke('start_recording');
            this.isRecording = true;
            this.button.innerHTML = '<span class="icon icon-mic-off"></span>';
        } catch (err) {
            console.error(err);
        }
    }

    async stop() {
        try {
            const wavData = await invoke('stop_recording');
            this.isRecording = false;
            this.button.innerHTML = '<span class="icon icon-mic-on"></span>';
            return new Uint8Array(wavData);
        } catch (err) {
            console.error(err);
            return null;
        }
    }
}

class VoiceTranscriptionUI {
    constructor() {
        this.selectedModel = 'base'; // Default model
        this.isSettingUp = false;
        this.autoTranslate = false;
        this.autoTranscript = false;
    }

        async ensureModelReady() {
        if (this.isSettingUp) return false;

        // Get current settings from localStorage
        this.autoTranslate = localStorage.getItem('autoTranslate') === 'true';
        this.autoTranscript = localStorage.getItem('autoTranscript') === 'true';
        
        const model = window.voiceSettings?.models?.find(m => m.name === this.selectedModel);
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

            await window.voiceSettings.downloadModel(this.selectedModel);
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

    async transcribeRecording(wavData) {
        if (!await this.ensureModelReady()) {
            throw new Error("Voice model setup failed");
        }

        if (!wavData) {
            throw new Error("No audio data to transcribe");
        }
        
        return await invoke('transcribe_audio', {
            audioData: Array.from(wavData),
            modelId: this.selectedModel,
            autoTranslate: this.autoTranslate
        });
    }

    async transcribeAudioFile(filePath) {
        if (!await this.ensureModelReady()) {
            throw new Error("Voice model setup failed");
        }

        return await invoke('transcribe', {
            filePath: filePath,
            modelName: this.selectedModel,
            autoTranslate: this.autoTranslate
        });
    }
}

// Initialize when DOM is ready
document.addEventListener('DOMContentLoaded', () => {
    // Initialize voice transcription with default model
    cTranscriber = new VoiceTranscriptionUI();
});

function handleAudioAttachment(cAttachment, assetUrl, pMessage) {
    if (['wav', 'mp3'].includes(cAttachment.extension)) {
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
        transcribeBtn.style.display = `flex`;

        const transcribeIcon = document.createElement('span');
        transcribeIcon.classList.add('icon', 'icon-mic-on');
        transcribeIcon.style.position = `relative`;
        transcribeIcon.style.backgroundColor = `rgba(255, 255, 255, 0.45)`;
        transcribeIcon.style.width = `19px`;
        transcribeIcon.style.height = `19px`;
        transcribeBtn.appendChild(transcribeIcon);
        
        const transcribeText = document.createElement('span');
        transcribeText.textContent = `Transcribe`;
        transcribeText.style.color = `rgba(255, 255, 255, 0.45)`;
        transcribeText.style.marginLeft = `5px`;
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
            transcribeText.textContent = `Transcribing`;
            transcribeBtn.style.cursor = 'default';
            transcribeIcon.classList.replace('icon-mic-on', 'icon-loading');
            transcribeIcon.classList.add('spin');

            try {
                // Get the audio file path and send to backend for transcription
                const transcription = await cTranscriber.transcribeAudioFile(cAttachment.path);
              
                // Remove the loading state (or, currently, the entire button)
                transcribeBtn.remove();
                
// Clear any existing content
                while (transcriptionResult.firstChild) {
                    transcriptionResult.removeChild(transcriptionResult.firstChild);
                }
                
                // Create transcription text container
                const transcriptionText = document.createElement('div');
                transcriptionText.classList.add('transcription-text');
                
                const p = document.createElement('span');
                p.textContent = transcription;
                transcriptionText.appendChild(p);
                
                transcriptionResult.appendChild(transcriptionText);
                transcriptionResult.classList.remove('hidden');
            } catch (err) {
                console.error('Transcription error:', err);
                
                // Clear any existing content
                while (transcriptionResult.firstChild) {
                    transcriptionResult.removeChild(transcriptionResult.firstChild);
                }
                
                const errorDiv = document.createElement('div');
                errorDiv.classList.add('transcription-error');
                errorDiv.textContent = `Error: ${err.message || 'Transcription failed'}`;
                transcriptionResult.appendChild(errorDiv);
                transcriptionResult.classList.remove('hidden');
            } finally {
                transcribeIcon.classList.replace('icon-loading', 'icon-mic-on');
                transcribeBtn.disabled = false;
            }
        });

        transcribeContainer.appendChild(transcribeBtn);
        audioContainer.appendChild(audPreview);
        audioContainer.appendChild(transcribeContainer);
        audioContainer.appendChild(transcriptionResult);
        pMessage.appendChild(audioContainer);
    }
}