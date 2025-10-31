/**
 * Chat Scroll Management
 * Handles procedural message loading, scroll correction, and navigation
 */

// Procedural scroll state
const proceduralScrollState = {
    isLoading: false,
    renderedMessageCount: 0,
    totalMessageCount: 0,
    messagesPerBatch: 20,
    scrollThreshold: 300, // pixels from top to trigger load
    lastScrollHeight: 0, // Track scroll height for media load correction
    isLoadingOlderMessages: false // Flag to indicate we're in procedural load mode
};

/**
 * Correct scroll position when media loads during procedural scroll
 * This prevents "snap-back" when images/videos load after messages are rendered
 */
function correctScrollForMediaLoad() {
    // Only correct if we're not at the bottom and we have a baseline
    if (!proceduralScrollState.lastScrollHeight || !domChatMessages) return;
    
    const currentScrollHeight = domChatMessages.scrollHeight;
    const scrollHeightDiff = currentScrollHeight - proceduralScrollState.lastScrollHeight;
    
    // Only correct if there's a meaningful difference (media loaded)
    if (scrollHeightDiff > 5) {
        const currentScrollTop = domChatMessages.scrollTop;
        domChatMessages.scrollTop = currentScrollTop + scrollHeightDiff;
        proceduralScrollState.lastScrollHeight = currentScrollHeight;
    }
}

/**
 * Handle procedural scroll loading of older messages
 */
function handleProceduralScroll() {
    if (!strOpenChat || proceduralScrollState.isLoading) return;

    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || !chat.messages) return;

    // Check if user has scrolled near the top
    const scrollTop = domChatMessages.scrollTop;
    if (scrollTop > proceduralScrollState.scrollThreshold) return;

    // Check if there are more messages to load
    const totalMessages = chat.messages.length;
    if (proceduralScrollState.renderedMessageCount >= totalMessages) return;

    // Load more messages
    loadMoreMessages();
}

/**
 * Load the next batch of older messages
 */
async function loadMoreMessages() {
    if (proceduralScrollState.isLoading || !strOpenChat) return;

    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || !chat.messages) return;

    const totalMessages = chat.messages.length;
    const currentRendered = proceduralScrollState.renderedMessageCount;

    if (currentRendered >= totalMessages) return;

    proceduralScrollState.isLoading = true;
    proceduralScrollState.isLoadingOlderMessages = true;

    // Calculate how many more messages to load
    const messagesToLoad = Math.min(
        proceduralScrollState.messagesPerBatch,
        totalMessages - currentRendered
    );

    // Get the next batch of older messages
    const startIndex = totalMessages - currentRendered - messagesToLoad;
    const endIndex = totalMessages - currentRendered;
    const olderMessages = chat.messages.slice(startIndex, endIndex);

    if (olderMessages.length === 0) {
        proceduralScrollState.isLoading = false;
        proceduralScrollState.isLoadingOlderMessages = false;
        return;
    }

    // Store scroll position BEFORE rendering
    const scrollHeightBefore = domChatMessages.scrollHeight;
    const scrollTopBefore = domChatMessages.scrollTop;

    // Get profile for rendering
    const isGroup = chat?.chat_type === 'MlsGroup';
    const profile = !isGroup ? getProfile(chat.id) : null;

    // Render the older messages
    await updateChat(chat, olderMessages, profile, false);

    // Update rendered count
    proceduralScrollState.renderedMessageCount += messagesToLoad;

    // Correct scroll position to prevent "snapping"
    const scrollHeightAfter = domChatMessages.scrollHeight;
    const scrollHeightDiff = scrollHeightAfter - scrollHeightBefore;
    
    // Adjust scroll position to maintain visual position
    domChatMessages.scrollTop = scrollTopBefore + scrollHeightDiff;
    
    // Store the current scroll height for media load correction
    proceduralScrollState.lastScrollHeight = domChatMessages.scrollHeight;

    proceduralScrollState.isLoading = false;
    
    // Keep the flag active for a bit longer to catch late-loading media
    setTimeout(() => {
        proceduralScrollState.isLoadingOlderMessages = false;
        proceduralScrollState.lastScrollHeight = 0;
    }, 2000);
}

/**
 * Initialize procedural scroll state for a chat
 * @param {Object} chat - The chat object
 */
function initProceduralScroll(chat) {
    if (!chat || !chat.messages) {
        proceduralScrollState.renderedMessageCount = 0;
        proceduralScrollState.totalMessageCount = 0;
        return;
    }

    const totalMessages = chat.messages.length;
    proceduralScrollState.totalMessageCount = totalMessages;
    
    // Start by rendering the most recent batch
    const initialBatch = Math.min(proceduralScrollState.messagesPerBatch, totalMessages);
    proceduralScrollState.renderedMessageCount = initialBatch;
}

/**
 * Reset procedural scroll state (call when closing chat)
 */
function resetProceduralScroll() {
    proceduralScrollState.isLoading = false;
    proceduralScrollState.renderedMessageCount = 0;
    proceduralScrollState.totalMessageCount = 0;
    proceduralScrollState.lastScrollHeight = 0;
    proceduralScrollState.isLoadingOlderMessages = false;
}

/**
 * Wait for all media elements in the chat to finish loading
 * @param {number} timeout - Maximum time to wait in milliseconds (default: 5000)
 */
async function waitForMediaToLoad(timeout = 5000) {
    // Get all media elements in the chat
    const images = Array.from(domChatMessages.querySelectorAll('img'));
    const videos = Array.from(domChatMessages.querySelectorAll('video'));
    const allMedia = [...images, ...videos];
    
    if (allMedia.length === 0) return;
    
    // Create promises for each media element
    const mediaPromises = allMedia.map(media => {
        return new Promise((resolve) => {
            // If already loaded, resolve immediately
            if (media instanceof HTMLImageElement) {
                if (media.complete && media.naturalHeight !== 0) {
                    resolve();
                    return;
                }
            } else if (media instanceof HTMLVideoElement) {
                if (media.readyState >= 1) { // HAVE_METADATA or better
                    resolve();
                    return;
                }
            }
            
            // Set up load event listeners
            const onLoad = () => {
                cleanup();
                resolve();
            };
            
            const onError = () => {
                cleanup();
                resolve(); // Resolve anyway to not block
            };
            
            const cleanup = () => {
                media.removeEventListener('load', onLoad);
                media.removeEventListener('loadedmetadata', onLoad);
                media.removeEventListener('error', onError);
            };
            
            // Add listeners
            if (media instanceof HTMLImageElement) {
                media.addEventListener('load', onLoad, { once: true });
                media.addEventListener('error', onError, { once: true });
            } else if (media instanceof HTMLVideoElement) {
                media.addEventListener('loadedmetadata', onLoad, { once: true });
                media.addEventListener('error', onError, { once: true });
            }
            
            // Timeout fallback
            setTimeout(() => {
                cleanup();
                resolve();
            }, timeout);
        });
    });
    
    // Wait for all media to load or timeout
    await Promise.race([
        Promise.all(mediaPromises),
        new Promise(resolve => setTimeout(resolve, timeout))
    ]);
    
    // Add a small buffer for layout stabilization
    await new Promise(resolve => setTimeout(resolve, 100));
}

/**
 * Load and scroll to a specific message that isn't currently rendered
 * @param {string} targetMsgId - The ID of the message to scroll to
 */
async function loadAndScrollToMessage(targetMsgId) {
    if (!strOpenChat) return;
    
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || !chat.messages) return;
    
    // Find the target message in the chat
    const targetMsgIndex = chat.messages.findIndex(m => m.id === targetMsgId);
    if (targetMsgIndex === -1) return console.warn('Target message not found in chat history');
    
    // Calculate which messages to load:
    // - All messages from the target to the most recent
    // - Plus 20 additional older messages for context
    const contextMessages = 20;
    const startIndex = Math.max(0, targetMsgIndex - contextMessages);
    const endIndex = chat.messages.length;
    
    const messagesToLoad = chat.messages.slice(startIndex, endIndex);
    
    // Update procedural scroll state to reflect what we're about to render
    proceduralScrollState.renderedMessageCount = messagesToLoad.length;
    proceduralScrollState.totalMessageCount = chat.messages.length;
    
    // Get profile for rendering
    const isGroup = chat?.chat_type === 'MlsGroup';
    const profile = !isGroup ? getProfile(chat.id) : null;
    
    // Clear existing messages and render the new range
    while (domChatMessages.firstElementChild) {
        domChatMessages.firstElementChild.remove();
    }
    
    // Render all the messages
    await updateChat(chat, messagesToLoad, profile, false);
    
    // Wait for all media (images, videos) to load before scrolling
    await waitForMediaToLoad();
    
    // Now scroll to the target message
    const domMsg = document.getElementById(targetMsgId);
    if (domMsg) {
        centerInView(domMsg);
        
        // Run an animation to bring the user's eye to the message
        const pContainer = domMsg.querySelector('p');
        if (pContainer && !pContainer.classList.contains('no-background')) {
            domMsg.classList.add('highlight-animation');
            setTimeout(() => domMsg.classList.remove('highlight-animation'), 1500);
        }
    }
}