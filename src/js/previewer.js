/**
 * Image Viewer with Zoom and Pan
 * Allows users to click on images to view them in fullscreen
 * with zoom (scroll/pinch) and pan (drag/touch-drag) capabilities
 */

let viewerOverlay = null;
let viewerImage = null;
let viewerContainer = null;
let zoomInfo = null;

// Zoom and pan state
let scale = 1;
let translateX = 0;
let translateY = 0;
let isDragging = false;
let startX = 0;
let startY = 0;
let lastTouchDistance = 0;

/**
 * Create the image viewer overlay
 */
function createViewer() {
    // Create overlay
    viewerOverlay = document.createElement('div');
    viewerOverlay.className = 'image-viewer-overlay';
    
    // Create container
    viewerContainer = document.createElement('div');
    viewerContainer.className = 'image-viewer-container';
    
    // Create image
    viewerImage = document.createElement('img');
    viewerImage.className = 'image-viewer-image';
    viewerImage.draggable = false; // Disable native drag-and-drop
    
    // Create close button
    const closeBtn = document.createElement('button');
    closeBtn.className = 'image-viewer-close';
    closeBtn.setAttribute('aria-label', 'Close');
    
    // Create zoom info
    zoomInfo = document.createElement('div');
    zoomInfo.className = 'image-viewer-zoom-info';
    zoomInfo.textContent = '100%';
    
    // Assemble
    viewerContainer.appendChild(viewerImage);
    viewerOverlay.appendChild(viewerContainer);
    viewerOverlay.appendChild(closeBtn);
    viewerOverlay.appendChild(zoomInfo);
    document.body.appendChild(viewerOverlay);
    
    // Event listeners
    closeBtn.addEventListener('click', closeViewer);
    
    // Close on background click (but not if user was dragging)
    let clickStartX = 0;
    let clickStartY = 0;
    let wasDragging = false;
    
    viewerContainer.addEventListener('mousedown', (e) => {
        if (e.target === viewerContainer) {
            clickStartX = e.clientX;
            clickStartY = e.clientY;
            wasDragging = false;
        }
    });
    
    viewerContainer.addEventListener('mousemove', (e) => {
        if (e.target === viewerContainer) {
            const moveDistance = Math.hypot(e.clientX - clickStartX, e.clientY - clickStartY);
            if (moveDistance > 5) wasDragging = true;
        }
    });
    
    viewerContainer.addEventListener('mouseup', (e) => {
        if (e.target === viewerContainer && !wasDragging) {
            closeViewer();
        }
    });
    
    // Also handle touch for mobile
    viewerContainer.addEventListener('touchstart', (e) => {
        if (e.target === viewerContainer && e.touches.length === 1) {
            clickStartX = e.touches[0].clientX;
            clickStartY = e.touches[0].clientY;
            wasDragging = false;
        }
    });
    
    viewerContainer.addEventListener('touchmove', (e) => {
        if (e.target === viewerContainer && e.touches.length === 1) {
            const moveDistance = Math.hypot(e.touches[0].clientX - clickStartX, e.touches[0].clientY - clickStartY);
            if (moveDistance > 5) wasDragging = true;
        }
    });
    
    viewerContainer.addEventListener('touchend', (e) => {
        if (e.target === viewerContainer && !wasDragging) {
            closeViewer();
        }
    });
    
    // Keyboard support
    document.addEventListener('keydown', handleKeyDown);
    
    // Mouse wheel zoom
    viewerContainer.addEventListener('wheel', handleWheel, { passive: false });
    
    // Mouse drag
    viewerImage.addEventListener('mousedown', handleMouseDown);
    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    
    // Touch support
    viewerImage.addEventListener('touchstart', handleTouchStart, { passive: false });
    viewerImage.addEventListener('touchmove', handleTouchMove, { passive: false });
    viewerImage.addEventListener('touchend', handleTouchEnd);
}

/**
 * Open image in viewer
 */
function openImageViewer(imageSrc) {
    if (!viewerOverlay) createViewer();
    
    // Reset state
    scale = 1;
    translateX = 0;
    translateY = 0;
    isDragging = false;
    
    // Set image
    viewerImage.src = imageSrc;
    viewerImage.style.transform = 'translate(0, 0) scale(1)';
    
    // Show overlay
    viewerOverlay.style.display = 'flex';
    setTimeout(() => viewerOverlay.classList.add('active'), 10);
    
    // Update zoom info
    updateZoomInfo();
}

/**
 * Close viewer
 */
function closeViewer() {
    if (!viewerOverlay) return;
    
    viewerOverlay.classList.remove('active');
    setTimeout(() => {
        viewerOverlay.style.display = 'none';
    }, 200);
}

/**
 * Handle keyboard events
 */
function handleKeyDown(e) {
    if (!viewerOverlay || viewerOverlay.style.display === 'none') return;
    
    if (e.key === 'Escape') {
        closeViewer();
    }
}

/**
 * Handle mouse wheel zoom
 */
function handleWheel(e) {
    e.preventDefault();
    
    const delta = e.deltaY > 0 ? 0.9 : 1.1;
    const newScale = Math.min(Math.max(0.5, scale * delta), 5);
    
    // If we're at 1x zoom or going back to it, keep centered
    if (scale === 1 || newScale === 1) {
        scale = newScale;
        translateX = 0;
        translateY = 0;
    } else {
        // When already zoomed, zoom towards cursor position
        const rect = viewerImage.getBoundingClientRect();
        const offsetX = e.clientX - rect.left - rect.width / 2;
        const offsetY = e.clientY - rect.top - rect.height / 2;
        
        const scaleChange = newScale / scale;
        translateX = translateX * scaleChange + offsetX * (1 - scaleChange);
        translateY = translateY * scaleChange + offsetY * (1 - scaleChange);
        
        scale = newScale;
    }
    
    updateTransform();
    updateZoomInfo();
}

/**
 * Handle mouse drag start
 */
function handleMouseDown(e) {
    isDragging = true;
    startX = e.clientX - translateX;
    startY = e.clientY - translateY;
    viewerImage.classList.add('dragging');
}

/**
 * Handle mouse drag move
 */
function handleMouseMove(e) {
    if (!isDragging) return;
    
    translateX = e.clientX - startX;
    translateY = e.clientY - startY;
    updateTransform();
}

/**
 * Handle mouse drag end
 */
function handleMouseUp() {
    isDragging = false;
    viewerImage.classList.remove('dragging');
}

/**
 * Handle touch start
 */
function handleTouchStart(e) {
    if (e.touches.length === 1) {
        // Single touch - prepare for drag
        isDragging = true;
        startX = e.touches[0].clientX - translateX;
        startY = e.touches[0].clientY - translateY;
        viewerImage.classList.add('dragging');
    } else if (e.touches.length === 2) {
        // Two touches - prepare for pinch zoom
        e.preventDefault();
        const touch1 = e.touches[0];
        const touch2 = e.touches[1];
        lastTouchDistance = Math.hypot(
            touch2.clientX - touch1.clientX,
            touch2.clientY - touch1.clientY
        );
    }
}

/**
 * Handle touch move
 */
function handleTouchMove(e) {
    if (e.touches.length === 1 && isDragging) {
        // Single touch drag
        e.preventDefault();
        translateX = e.touches[0].clientX - startX;
        translateY = e.touches[0].clientY - startY;
        updateTransform();
    } else if (e.touches.length === 2) {
        // Pinch zoom
        e.preventDefault();
        const touch1 = e.touches[0];
        const touch2 = e.touches[1];
        const distance = Math.hypot(
            touch2.clientX - touch1.clientX,
            touch2.clientY - touch1.clientY
        );
        
        if (lastTouchDistance > 0) {
            const delta = distance / lastTouchDistance;
            const newScale = Math.min(Math.max(0.5, scale * delta), 5);
            
            // Calculate center point between touches
            const centerX = (touch1.clientX + touch2.clientX) / 2;
            const centerY = (touch1.clientY + touch2.clientY) / 2;
            
            // Adjust translation to zoom towards center
            const scaleChange = newScale / scale;
            translateX = centerX - (centerX - translateX) * scaleChange;
            translateY = centerY - (centerY - translateY) * scaleChange;
            
            scale = newScale;
            updateTransform();
            updateZoomInfo();
        }
        
        lastTouchDistance = distance;
    }
}

/**
 * Handle touch end
 */
function handleTouchEnd(e) {
    if (e.touches.length < 2) {
        lastTouchDistance = 0;
    }
    if (e.touches.length === 0) {
        isDragging = false;
        viewerImage.classList.remove('dragging');
    }
}

/**
 * Update image transform
 */
function updateTransform() {
    // Apply bounds checking to prevent image from going off-screen
    const rect = viewerImage.getBoundingClientRect();
    const containerWidth = viewerContainer.clientWidth;
    const containerHeight = viewerContainer.clientHeight;
    
    // Calculate the scaled dimensions
    const scaledWidth = rect.width / scale * scale;
    const scaledHeight = rect.height / scale * scale;
    
    // Calculate max translation bounds
    const maxTranslateX = Math.max(0, (scaledWidth - containerWidth) / 2);
    const maxTranslateY = Math.max(0, (scaledHeight - containerHeight) / 2);
    
    // Constrain translation within bounds
    translateX = Math.max(-maxTranslateX, Math.min(maxTranslateX, translateX));
    translateY = Math.max(-maxTranslateY, Math.min(maxTranslateY, translateY));
    
    viewerImage.style.transform = `translate(${translateX}px, ${translateY}px) scale(${scale})`;
    viewerImage.classList.toggle('zoomed', scale > 1);
}

/**
 * Update zoom info display
 */
function updateZoomInfo() {
    const percent = Math.round(scale * 100);
    zoomInfo.textContent = `${percent}%`;
    zoomInfo.classList.add('visible');
    
    // Hide after 1 second
    setTimeout(() => {
        zoomInfo.classList.remove('visible');
    }, 1000);
}

/**
 * Attach click handler to an image element
 * Call this when rendering images in the chat
 */
function attachImagePreview(imgElement) {
    if (!imgElement || imgElement.dataset.previewAttached) return;
    
    // Add btn class for pointer cursor and hover effects
    imgElement.classList.add('btn');
    
    imgElement.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (imgElement.src && !imgElement.src.startsWith('data:')) {
            openImageViewer(imgElement.src);
        }
    });
    
    // Mark as attached to avoid duplicate handlers
    imgElement.dataset.previewAttached = 'true';
}