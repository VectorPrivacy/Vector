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
        this.models = [
            { id: 'tiny', name: 'Tiny', description: 'Fastest, least accurate', downloaded: false, downloading: false },
            { id: 'base', name: 'Base', description: 'Fast, decent accuracy', downloaded: false, downloading: false },
            { id: 'small', name: 'Small', description: 'Slower, better accuracy', downloaded: false, downloading: false },
            { id: 'medium', name: 'Medium', description: 'Slow, good accuracy', downloaded: false, downloading: false },
            { id: 'large', name: 'Large', description: 'Very slow, best accuracy', downloaded: false, downloading: false }
        ];
        this.selectedModel = 'base'; // Default model
        this.initUI();
        this.checkDownloadedModels();
    }

    initUI() {
        if (!document.querySelector('.settings-section-voice')) {
            const settingsSection = document.createElement('div');
            settingsSection.className = 'settings-section settings-section-voice';
            settingsSection.innerHTML = `
                <h3>Voice Settings</h3>
                <div class="form-group">
                    <label for="whisper-model">Whisper Model</label>
                    <select id="whisper-model" class="form-control">
                        ${this.models.map(model => 
                            `<option value="${model.id}">${model.name} (${model.description})</option>`
                        ).join('')}
                    </select>
                    <div id="model-status" class="model-status"></div>
                </div>
                <div class="form-group">
                    <button id="download-model" class="btn btn-primary">Download Selected Model</button>
                </div>
                <div class="form-group">
                    <button id="transcribe-btn" class="btn btn-success">Transcribe Recording</button>
                    <div id="transcription-result" class="transcription-result"></div>
                </div>
            `;
            
            // Insert into settings container (adjust selector as needed)
            document.querySelector('.settings-container').appendChild(settingsSection);
            
            // Add event listeners
            document.getElementById('whisper-model').addEventListener('change', (e) => {
                this.selectedModel = e.target.value;
                this.updateModelStatus();
            });
            
            document.getElementById('download-model').addEventListener('click', () => {
                this.downloadModel(this.selectedModel);
            });
            
            document.getElementById('transcribe-btn').addEventListener('click', async () => {
                await this.transcribeRecording();
            });
             // Ensure the dropdown visually matches our selected model
        const dropdown = document.getElementById('whisper-model');
        if (dropdown) {
            dropdown.value = this.selectedModel;
        }
        }
        
        this.updateModelStatus();
    }

    async checkDownloadedModels() {
        try {
            const downloadedModels = await invoke('get_downloaded_models');
            this.models.forEach(model => {
                model.downloaded = downloadedModels.includes(model.id);
            });
            this.updateModelStatus();
        } catch (err) {
            console.error('Error checking downloaded models:', err);
        }
    }

    updateModelStatus() {
        const statusElement = document.getElementById('model-status');
        if (!statusElement) return;
        
        const model = this.models.find(m => m.id === this.selectedModel);
        if (!model) return;
        
        if (model.downloading) {
            statusElement.innerHTML = `<div class="alert alert-info">Downloading ${model.name} model...</div>`;
            return;
        }
        
        if (model.downloaded) {
            statusElement.innerHTML = `<div class="alert alert-success">${model.name} model is downloaded and ready</div>`;
        } else {
            statusElement.innerHTML = `<div class="alert alert-warning">${model.name} model is not downloaded</div>`;
        }
    }

    async downloadModel(modelId) {
        const model = this.models.find(m => m.id === modelId);
        if (!model || model.downloading || model.downloaded) return;
        
        model.downloading = true;
        this.updateModelStatus();
        
        try {
            await invoke('download_model', { modelId });
            model.downloaded = true;
        } catch (err) {
            console.error('Error downloading model:', err);
        } finally {
            model.downloading = false;
            this.updateModelStatus();
        }
    }

    async transcribeRecording() {
        const resultElement = document.getElementById('transcription-result');
        const transcribeBtn = document.getElementById('transcribe-btn');
        
        if (!resultElement || !transcribeBtn) return;
        
        resultElement.innerHTML = '<div class="alert alert-info">Transcribing...</div>';
        transcribeBtn.disabled = true;
        
        try {
            // Get the recorded audio (assuming VoiceRecorder is available)
            const wavData = await voiceRecorder.stop();
            if (!wavData) {
                resultElement.innerHTML = '<div class="alert alert-danger">No audio data to transcribe</div>';
                return;
            }
            
            // Transcribe using the selected model
            const transcription = await invoke('transcribe_audio', {
                audioData: Array.from(wavData),
                modelId: this.selectedModel
            });
            
            resultElement.innerHTML = `
                <div class="alert alert-success">
                    <strong>Transcription:</strong>
                    <p>${transcription}</p>
                </div>
            `;
        } catch (err) {
            console.error('Transcription error:', err);
            resultElement.innerHTML = `<div class="alert alert-danger">Error: ${err.message || err}</div>`;
        } finally {
            transcribeBtn.disabled = false;
        }
    }
}

// Initialize when DOM is ready
document.addEventListener('DOMContentLoaded', () => {
    // Initialize voice recorder if not already done
    const micButton = document.querySelector('.mic-button'); // Adjust selector
    if (micButton && !window.voiceRecorder) {
        window.voiceRecorder = new VoiceRecorder(micButton);
    }
    
    // Initialize transcription UI
    window.voiceTranscriptionUI = new VoiceTranscriptionUI();
});