/**
 * Creates a video element optimized for Android WebView
 * Android WebView requires special handling due to lack of range request support in asset protocol
 * 
 * @param {string} assetUrl - The asset:// URL of the video
 * @param {Object} cAttachment - Attachment object containing metadata
 * @param {Function} onMetadataLoaded - Callback when video metadata is loaded
 * @param {Function} callback - Callback that receives the created element
 */
function createAndroidVideo(assetUrl, cAttachment, onMetadataLoaded, callback) {
    // Fetch the video
    fetch(assetUrl)
        .then(response => {
            if (!response.ok) {
                throw new Error(`Failed to fetch video: ${response.status}`);
            }
            return response.blob();
        })
        .then(blob => {
            // Create video element
            const vidPreview = document.createElement('video');
            vidPreview.setAttribute('controlsList', 'nodownload');
            vidPreview.controls = true;
            vidPreview.style.width = `100%`;
            vidPreview.style.height = `auto`;
            vidPreview.style.borderRadius = `8px`;
            vidPreview.style.cursor = `pointer`;
            vidPreview.preload = "metadata";
            vidPreview.playsInline = true;
            
            const blobUrl = URL.createObjectURL(blob);
            vidPreview.src = blobUrl;
            
            // Handle metadata loaded event
            if (onMetadataLoaded) {
                vidPreview.addEventListener('loadedmetadata', () => {
                    onMetadataLoaded(vidPreview);
                }, { once: true });
            }
            
            callback(vidPreview);
        })
        .catch(err => {
            // Create error element
            const errorDiv = document.createElement('div');
            errorDiv.style.cssText = `
                width: 100%;
                padding: 30px 20px;
                background: #fff3cd;
                border-radius: 8px;
                text-align: center;
                color: #856404;
                border: 1px solid #ffeaa7;
            `;
            
            const errorMessage = document.createElement('i');
            errorMessage.textContent = 'Failed to Load Video';
            
            errorDiv.appendChild(errorMessage);
            callback(errorDiv);
        });
}