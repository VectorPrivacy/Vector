/**
 * Image Viewer with Zoom and Pan
 * Allows users to click on images to view them in fullscreen
 * with zoom (scroll/pinch) and pan (drag/touch-drag) capabilities
 */

// The element refs the arithmetic measures (the component binds them).
const viewerImage = () => VectorSvelte.imageViewerEls().image;
const viewerContainer = () => VectorSvelte.imageViewerEls().container;
let viewerOpen = false;
let tipTimer = null;

// Zoom and pan state
let scale = 1;
let translateX = 0;
let translateY = 0;
let rotation = 0; // quarter-turns in degrees (0/90/180/270), CCW via the rotate control
let isDragging = false;
let startX = 0;
let startY = 0;
let lastTouchDistance = 0;
let zoomInfoTimeout = null;
let wheelSettleTimer = null;
let baseWidth = 0;
let baseHeight = 0;

// The viewer paints from the store; what stays here is the zoom and pan arithmetic,
// registered as the component's handlers, plus the document-level listeners.
VectorSvelte.setImageViewerHandlers({
    close: () => closeViewer(),
    rotate: () => rotateCCW(),
    load: () => {
        measureBaseSize();
        centerImage();
        updateTransform();
        VectorSvelte.flushSync();
        void viewerImage()?.offsetWidth;   // commit the settled transform before re-arming the transition
        VectorSvelte.setImageViewer({ settling: false });
    },
    error: () => VectorSvelte.setImageViewer({ settling: false }),
    wheel: (e) => handleWheel(e),
    mouseDown: (e) => handleMouseDown(e),
    touchStart: (e) => handleTouchStart(e),
    touchMove: (e) => handleTouchMove(e),
    touchEnd: (e) => handleTouchEnd(e),
});
document.addEventListener('keydown', handleKeyDown);
document.addEventListener('mousemove', handleMouseMove);
document.addEventListener('mouseup', handleMouseUp);
window.addEventListener('resize', () => {
    if (viewerOpen && baseWidth > 0) {
        measureBaseSize();
        updateTransform();
    }
});

/**
 * Measure the base size of the image at scale 1.
 * The rect is the VISUAL (post-rotation) box, so a quarter-turned image
 * reports swapped axes — un-swap to keep base dims in the image's own space.
 */
function measureBaseSize() {
    const rect = viewerImage().getBoundingClientRect();
    const odd = (((rotation % 360) + 360) % 360) % 180 !== 0;
    baseWidth = (odd ? rect.height : rect.width) / scale;
    baseHeight = (odd ? rect.width : rect.height) / scale;
}

/**
 * Center the image's visual box in the container at the current scale/rotation.
 */
function centerImage() {
    const odd = (((rotation % 360) + 360) % 360) % 180 !== 0;
    const effW = (odd ? baseHeight : baseWidth) * scale;
    const effH = (odd ? baseWidth : baseHeight) * scale;
    translateX = (viewerContainer().clientWidth - effW) / 2;
    translateY = (viewerContainer().clientHeight - effH) / 2;
}

/**
 * Rotate the image a quarter-turn counterclockwise. The angle ACCUMULATES
 * (-90 each press, never normalized for the CSS emit) so consecutive turns
 * always interpolate the short way round instead of snapping the long way.
 * A rotation recenters — the axes swap, so the old position is meaningless.
 */
function rotateCCW() {
    rotation -= 90;
    centerImage();
    updateTransform();
}

/**
 * The corrective transform that puts the ROTATED image's visual top-left back at
 * the local origin, so translateX/Y and the clamping math keep meaning
 * "visual box position" at every rotation. Units are pre-scale (composed after
 * scale(), so they ride the same scaling).
 *
 * ALWAYS emits the full translate+rotate pair: a transform list that changes
 * shape between frames forces WebKit into matrix interpolation, which sweeps
 * the image around wildly mid-transition.
 */
function rotationFix() {
    const r = ((rotation % 360) + 360) % 360;
    if (r === 90) return ` translate(${baseHeight}px, 0px) rotate(${rotation}deg)`;
    if (r === 180) return ` translate(${baseWidth}px, ${baseHeight}px) rotate(${rotation}deg)`;
    if (r === 270) return ` translate(0px, ${baseWidth}px) rotate(${rotation}deg)`;
    return ` translate(0px, 0px) rotate(${rotation}deg)`;
}

/**
 * Open image in viewer
 */
function openImageViewer(imageSrc) {
    scale = 1;
    translateX = 0;
    translateY = 0;
    rotation = 0;
    isDragging = false;
    baseWidth = 0;
    baseHeight = 0;
    viewerOpen = true;

    // Hidden and unanimated until the first settled frame: the image otherwise paints at
    // the container's top-left and slides to centre once the load measures it.
    VectorSvelte.setImageViewer({ open: true, active: false, src: imageSrc, settling: true, zoomed: false, transform: 'translate(0, 0) scale(1)' });
    VectorSvelte.setImageViewerTip(platformFeatures.is_mobile ? 'Pinch to zoom' : 'Scroll to zoom', false);
    setTimeout(() => VectorSvelte.setImageViewer({ active: true }), 10);

    // Android back closes the viewer instead of navigating the conversation away.
    pushBack('image-viewer', closeViewer);

    updateZoomInfo();

    clearTimeout(tipTimer);
    tipTimer = setTimeout(() => {
        VectorSvelte.setImageViewerTip(VectorSvelte.imageViewerState().tip.text, true);
        tipTimer = setTimeout(() => VectorSvelte.setImageViewerTip(VectorSvelte.imageViewerState().tip.text, false), 2500);
    }, 500);
}

/**
 * Close viewer
 */
function closeViewer() {
    if (!viewerOpen) return;
    viewerOpen = false;
    popBack('image-viewer');
    VectorSvelte.setImageViewer({ active: false });
    setTimeout(() => { if (!viewerOpen) VectorSvelte.setImageViewer({ open: false, src: '' }); }, 200);
}

/**
 * Handle keyboard events
 */
function handleKeyDown(e) {
    if (!viewerOpen) return;

    if (e.key === 'Escape') {
        closeViewer();
    }
}

/**
 * Handle mouse wheel zoom
 */
function handleWheel(e) {
    e.preventDefault();

    // Continuous-input zoom: scale by the event's ACTUAL delta. A trackpad pinch
    // streams tiny, sign-noisy deltas — a fixed ±10% per event turned that into
    // rapid back-and-forth lurching. Exponential mapping makes tiny deltas tiny
    // steps; the clamp keeps one notch of a classic wheel near the old step size.
    const dy = e.deltaY * (e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 160 : 1);
    const factor = Math.min(1.15, Math.max(0.87, Math.exp(-dy * 0.0035)));
    const newScale = Math.min(Math.max(0.5, scale * factor), 5);

    // The 0.1s transition fights a high-frequency stream (every event retargets
    // the animation → rubber-banding). Off while zooming, re-armed once quiet.
    VectorSvelte.setImageViewer({ noAnim: true });
    clearTimeout(wheelSettleTimer);
    wheelSettleTimer = setTimeout(() => VectorSvelte.setImageViewer({ noAnim: false }), 150);
    
    // Get cursor position relative to the container
    const containerRect = viewerContainer().getBoundingClientRect();
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
    VectorSvelte.setImageViewer({ dragging: true });
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
    VectorSvelte.setImageViewer({ dragging: false });
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
        VectorSvelte.setImageViewer({ dragging: true });
    } else if (e.touches.length === 2) {
        // Stop dragging when pinching starts
        isDragging = false;
        VectorSvelte.setImageViewer({ dragging: false });
        // Pinch is a continuous stream too — the transition would rubber-band it
        VectorSvelte.setImageViewer({ noAnim: true });
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
            const containerRect = viewerContainer().getBoundingClientRect();
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
        VectorSvelte.setImageViewer({ noAnim: false });
    }
    if (e.touches.length === 0) {
        isDragging = false;
        VectorSvelte.setImageViewer({ dragging: false });
    }
}

/**
 * Update image transform
 */
function updateTransform() {
    const containerWidth = viewerContainer()?.clientWidth || 0;
    const containerHeight = viewerContainer()?.clientHeight || 0;

    // Use the base size (rendered size at scale 1) for calculations —
    // a quarter-turned image occupies swapped axes on screen
    const odd = (((rotation % 360) + 360) % 360) % 180 !== 0;
    const scaledWidth = (odd ? baseHeight : baseWidth) * scale;
    const scaledHeight = (odd ? baseWidth : baseHeight) * scale;
    
    // Per-axis: at rest (scale <= 1) a fitting axis snaps to center; while zoomed
    // in it only CLAMPS to stay on-screen — force-centering here was overwriting
    // the cursor-anchored position on every wheel/pinch event, so zoom always
    // gravitated to the middle instead of the cursor.
    const atRest = scale <= 1;
    if (scaledWidth <= containerWidth) {
        translateX = atRest
            ? (containerWidth - scaledWidth) / 2
            : Math.max(0, Math.min(containerWidth - scaledWidth, translateX));
    } else {
        // Overflowing axis: clamp so no gap opens at either edge
        translateX = Math.max(containerWidth - scaledWidth, Math.min(0, translateX));
    }

    if (scaledHeight <= containerHeight) {
        translateY = atRest
            ? (containerHeight - scaledHeight) / 2
            : Math.max(0, Math.min(containerHeight - scaledHeight, translateY));
    } else {
        translateY = Math.max(containerHeight - scaledHeight, Math.min(0, translateY));
    }
    
    VectorSvelte.setImageViewer({ transform: `translate(${translateX}px, ${translateY}px) scale(${scale})${rotationFix()}`, zoomed: scale > 1 });
}

/**
 * Update zoom info display
 */
function updateZoomInfo() {
    VectorSvelte.setImageViewerZoom(`${Math.round(scale * 100)}%`, true);
    // Hide after a second without zoom activity.
    if (zoomInfoTimeout) clearTimeout(zoomInfoTimeout);
    zoomInfoTimeout = setTimeout(() => {
        VectorSvelte.setImageViewerZoom(VectorSvelte.imageViewerState().zoom.text, false);
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