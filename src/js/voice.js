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
        
        const transcribeIcon = document.createElement('span');
        transcribeIcon.classList.add('icon', 'icon-mic');
        transcribeBtn.appendChild(transcribeIcon);
        
        const transcribeText = document.createTextNode(' View Transcription');
        transcribeBtn.appendChild(transcribeText);
        
        // Create container for transcription result
        const transcriptionResult = document.createElement('div');
        transcriptionResult.classList.add('transcription-result', 'hidden');

        transcribeBtn.addEventListener('click', async () => {
            // If already transcribed, just toggle visibility
            if (transcriptionResult.textContent.trim()) {
                transcriptionResult.classList.toggle('hidden');
                softChatScroll();
                return;
            }

            // Show loading state
            transcribeBtn.disabled = true;
            transcribeIcon.classList.replace('icon-mic', 'spinner');

            try {
                // Get the audio file path and send to backend for transcription
                const transcription = await window.voiceTranscriptionUI.transcribeAudioFile(cAttachment.path);
                
                // Clear any existing content
                while (transcriptionResult.firstChild) {
                    transcriptionResult.removeChild(transcriptionResult.firstChild);
                }
                
                // Create transcription text container
                const transcriptionText = document.createElement('div');
                transcriptionText.classList.add('transcription-text');
                
                const strong = document.createElement('strong');
                strong.textContent = 'Transcription:';
                transcriptionText.appendChild(strong);
                
                const p = document.createElement('p');
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
                transcribeIcon.classList.replace('spinner', 'icon-mic');
                transcribeBtn.disabled = false;
                softChatScroll();
            }
        });

        transcribeContainer.appendChild(transcribeBtn);
        audioContainer.appendChild(audPreview);
        audioContainer.appendChild(transcribeContainer);
        audioContainer.appendChild(transcriptionResult);
        pMessage.appendChild(audioContainer);
    }
}