/**
 * Image Viewer with Zoom and Pan
 * Allows users to click on images to view them in fullscreen
 * with zoom (scroll/pinch) and pan (drag/touch-drag) capabilities
 */

let viewerOverlay = null;
let viewerImage = null;
let viewerContainer = null;
let zoomInfo = null;
let zoomTip = null;

// Zoom and pan state
let scale = 1;
let translateX = 0;
let translateY = 0;
let isDragging = false;
let startX = 0;
let startY = 0;
let lastTouchDistance = 0;
let zoomInfoTimeout = null;
let baseWidth = 0;
let baseHeight = 0;

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
    
    // Create zoom tip
    zoomTip = document.createElement('div');
    zoomTip.className = 'image-viewer-tip';
    zoomTip.textContent = platformFeatures.is_mobile ? 'Pinch to zoom' : 'Scroll to zoom';
    
    // Assemble
    viewerContainer.appendChild(viewerImage);
    viewerOverlay.appendChild(viewerContainer);
    viewerOverlay.appendChild(closeBtn);
    viewerOverlay.appendChild(zoomInfo);
    viewerOverlay.appendChild(zoomTip);
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
    
    // Handle window resize and orientation changes
    window.addEventListener('resize', () => {
        if (viewerOverlay && viewerOverlay.style.display !== 'none' && baseWidth > 0) {
            measureBaseSize();
            updateTransform();
        }
    });
}

/**
 * Measure the base size of the image at scale 1
 */
function measureBaseSize() {
    const rect = viewerImage.getBoundingClientRect();
    baseWidth = rect.width / scale;
    baseHeight = rect.height / scale;
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
    baseWidth = 0;
    baseHeight = 0;
    
    // Set image
    viewerImage.src = imageSrc;
    viewerImage.style.transform = 'translate(0, 0) scale(1)';
    
    // Measure base size once image loads
    viewerImage.onload = () => {
        measureBaseSize();
        updateTransform();
    };
    
    // Show overlay
    viewerOverlay.style.display = 'flex';
    setTimeout(() => viewerOverlay.classList.add('active'), 10);
    
    // Update zoom info
    updateZoomInfo();
    
    // Show zoom tip briefly
    setTimeout(() => {
        zoomTip.classList.add('visible');
        setTimeout(() => {
            zoomTip.classList.remove('visible');
        }, 2500);
    }, 500);
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
    
    // Get cursor position relative to the container
    const containerRect = viewerContainer.getBoundingClientRect();
    const cursorX = e.clientX - containerRect.left;
    const cursorY = e.clientY - containerRect.top;
    
    // Calculate the point on the image that's under the cursor
    const imageX = (cursorX - translateX) / scale;
    const imageY = (cursorY - translateY) / scale;
    
    // Update scale
    scale = newScale;
    
    // Adjust translation to keep the same point under the cursor
    translateX = cursorX - imageX * scale;
    translateY = cursorY - imageY * scale;
    
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
        // Prevent scrolling if image is zoomed
        if (scale > 1) {
            e.preventDefault();
        }
        // Single touch - prepare for drag
        isDragging = true;
        startX = e.touches[0].clientX - translateX;
        startY = e.touches[0].clientY - translateY;
        viewerImage.classList.add('dragging');
    } else if (e.touches.length === 2) {
        // Stop dragging when pinching starts
        isDragging = false;
        viewerImage.classList.remove('dragging');
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
            
            // Get container's bounding rect to convert to container-relative coordinates
            const containerRect = viewerContainer.getBoundingClientRect();
            const centerX = (touch1.clientX + touch2.clientX) / 2;
            const centerY = (touch1.clientY + touch2.clientY) / 2;
            
            // Convert to container-relative coordinates
            const containerCenterX = centerX - containerRect.left;
            const containerCenterY = centerY - containerRect.top;
            
            // Calculate zoom origin relative to the image
            const originX = (containerCenterX - translateX) / scale;
            const originY = (containerCenterY - translateY) / scale;
            
            // Apply new scale and adjust translation to keep origin point fixed
            scale = newScale;
            translateX = containerCenterX - originX * scale;
            translateY = containerCenterY - originY * scale;
            
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
    const containerWidth = viewerContainer.clientWidth;
    const containerHeight = viewerContainer.clientHeight;
    
    // Use the base size (rendered size at scale 1) for calculations
    const scaledWidth = baseWidth * scale;
    const scaledHeight = baseHeight * scale;
    
    // Calculate bounds - when image is smaller than container, we want to center it
    const minTranslateX = Math.min(0, containerWidth - scaledWidth);
    const minTranslateY = Math.min(0, containerHeight - scaledHeight);
    
    if (scaledWidth <= containerWidth) {
        // Keep the image centered when it is smaller than the container
        translateX = (containerWidth - scaledWidth) / 2;
    } else {
        // Clamp translation so image doesn't go off-screen
        translateX = Math.max(minTranslateX, Math.min(0, translateX));
    }
    
    if (scaledHeight <= containerHeight) {
        // Keep the image centered when it is smaller than the container
        translateY = (containerHeight - scaledHeight) / 2;
    } else {
        // Clamp translation so image doesn't go off-screen
        translateY = Math.max(minTranslateY, Math.min(0, translateY));
    }
    
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
    
    // Clear any existing timeout to prevent flashing
    if (zoomInfoTimeout) clearTimeout(zoomInfoTimeout);
    
    // Hide after 1 second of no zoom activity
    zoomInfoTimeout = setTimeout(() => {
        zoomInfo.classList.remove('visible');
        zoomInfoTimeout = null;
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

/**
 * Create and attach a file extension badge to an image container
 * The badge shows the file extension and auto-hides if it's too large relative to the image
 * @param {HTMLImageElement} imgElement - The image element
 * @param {HTMLElement} container - The container to append the badge to (must have position: relative)
 * @param {string} extension - The file extension (without dot)
 */
function attachFileExtBadge(imgElement, container, extension) {
    // Check if display image types setting is enabled
    if (!fDisplayImageTypes) return null;
    if (!imgElement || !container || !extension) return null;

    const extBadge = document.createElement('span');
    extBadge.className = 'file-ext-badge';
    extBadge.textContent = extension.toUpperCase();
    // Initially hide until we check dimensions
    extBadge.style.display = 'none';

    // Check badge size after image loads
    imgElement.addEventListener('load', () => {
        const imgWidth = imgElement.offsetWidth;
        const imgHeight = imgElement.offsetHeight;

        // Show badge to measure it
        extBadge.style.display = '';
        const badgeWidth = extBadge.offsetWidth;
        const badgeHeight = extBadge.offsetHeight;

        // Hide badge if it's > 25% of image width or height
        const widthRatio = badgeWidth / imgWidth;
        const heightRatio = badgeHeight / imgHeight;

        if (widthRatio > 0.25 || heightRatio > 0.25) {
            extBadge.style.display = 'none';
            // Remove border radius from small images
            imgElement.style.borderRadius = '0';
        }
    }, { once: true });

    container.appendChild(extBadge);
    return extBadge;
}

/**
 * Extract file extension from a URL
 * @param {string} url - The URL to extract extension from
 * @returns {string|null} - The extension (without dot) or null
 */
function getExtensionFromUrl(url) {
    if (!url) return null;
    try {
        const urlObj = new URL(url);
        const path = urlObj.pathname.split('?')[0];
        const lastDot = path.lastIndexOf('.');
        if (lastDot === -1 || lastDot === path.length - 1) return null;
        return path.substring(lastDot + 1).toLowerCase();
    } catch {
        // Fallback for non-URL strings
        const lastDot = url.lastIndexOf('.');
        if (lastDot === -1 || lastDot === url.length - 1) return null;
        return url.substring(lastDot + 1).toLowerCase().split('?')[0];
    }
}