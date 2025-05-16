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
    }

    async transcribeRecording(wavData) {
        if (!wavData) {
            throw new Error("No audio data to transcribe");
        }
        
        return await invoke('transcribe_audio', {
            audioData: Array.from(wavData),
            modelId: this.selectedModel
        });
    }

    async transcribeAudioFile(filePath) {
        return await invoke('transcribe_audio_file', {
            filePath: filePath,
            modelId: this.selectedModel
        });
    }
}

// Initialize when DOM is ready
document.addEventListener('DOMContentLoaded', () => {
    // Initialize voice transcription with default model
    window.voiceTranscriptionUI = new VoiceTranscriptionUI();
});

function handleAudioAttachment(cAttachment, assetUrl, pMessage) {
    if (['wav', 'mp4'].includes(cAttachment.extension)) {
        const audioContainer = document.createElement('div');
        audioContainer.classList.add('audio-message-container');

        const audPreview = document.createElement('audio');
        audPreview.setAttribute('controlsList', 'nodownload');
        audPreview.controls = true;
        audPreview.preload = 'metadata';
        audPreview.src = assetUrl;
        audPreview.addEventListener('loadedmetadata', () => softChatScroll(), { once: true });

        // Add transcribe button for voice messages
        const transcribeBtn = document.createElement('button');
        transcribeBtn.classList.add('btn', 'btn-transcribe');
        transcribeBtn.innerHTML = '<span class="icon icon-mic"></span> Transcribe';

        // Create container for transcription result
        const transcriptionContainer = document.createElement('div');
        transcriptionContainer.classList.add('transcription-container', 'hidden');

        transcribeBtn.addEventListener('click', async () => {
            // Show loading state
            transcribeBtn.disabled = true;
            transcribeBtn.innerHTML = '<span class="spinner"></span> Transcribing...';

            try {
                // Get the audio file path and send to backend for transcription
                const transcription = await window.voiceTranscriptionUI.transcribeAudioFile(cAttachment.path);
                
                const resultDiv = document.createElement('div');
                resultDiv.classList.add('transcription-result');
                resultDiv.innerHTML = `<strong>Transcription:</strong><p>${transcription}</p>`;
                
                transcriptionContainer.innerHTML = '';
                transcriptionContainer.appendChild(resultDiv);
                transcriptionContainer.classList.remove('hidden');
            } catch (err) {
                console.error('Transcription error:', err);
                transcriptionContainer.innerHTML = 
                    `<div class="transcription-error">
                        Error: ${err.message || 'Transcription failed'}
                    </div>`;
                transcriptionContainer.classList.remove('hidden');
            } finally {
                transcribeBtn.innerHTML = '<span class="icon icon-mic"></span> Transcribe';
                transcribeBtn.disabled = false;
                softChatScroll();
            }
        });

        audioContainer.appendChild(audPreview);
        audioContainer.appendChild(transcribeBtn);
        audioContainer.appendChild(transcriptionContainer);
        pMessage.appendChild(audioContainer);
    }
}