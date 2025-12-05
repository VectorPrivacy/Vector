/**
 * Message Cache - LRU (Least Recently Used) Cache for Chat Messages
 * 
 * This module provides efficient on-demand message loading with automatic
 * memory management through LRU eviction.
 * 
 * LRU Cache Explanation:
 * - LRU = "Least Recently Used"
 * - When the cache is full and we need to add a new item, we evict the item
 *   that was accessed least recently (oldest access time)
 * - This ensures frequently accessed chats stay in memory while rarely
 *   accessed ones are automatically cleaned up
 * 
 * Benefits:
 * - Reduces RAM usage by not keeping all messages in memory
 * - Maintains fast access for recently opened chats
 * - Automatically manages memory without manual cleanup
 */

/**
 * Configuration for the message cache
 */
const MESSAGE_CACHE_CONFIG = {
    // Maximum number of chats to keep full message history for
    maxCachedChats: 5,
    
    // Maximum messages to keep per chat in cache
    maxMessagesPerChat: 100,
    
    // Number of messages to load per batch (for pagination)
    messagesPerBatch: 20,
    
    // Minimum messages to keep even after eviction (for preview)
    minMessagesForPreview: 1,
};

/**
 * Cache entry for a single chat's messages
 */
class ChatCacheEntry {
    constructor(chatId) {
        this.chatId = chatId;
        this.messages = [];
        this.lastAccess = Date.now();
        this.totalInDb = 0;  // Total message count in database
        this.loadedOffset = 0;  // How many messages from the end we've loaded
        this.isFullyLoaded = false;  // Whether all messages are loaded
    }
    
    /**
     * Update the last access time (for LRU tracking)
     */
    touch() {
        this.lastAccess = Date.now();
    }
    
    /**
     * Add messages to the cache (prepend older messages)
     * @param {Array} newMessages - Messages to add (should be older than existing)
     * @param {boolean} prepend - If true, add to beginning (older messages)
     */
    addMessages(newMessages, prepend = true) {
        if (prepend) {
            // Prepend older messages
            this.messages = [...newMessages, ...this.messages];
        } else {
            // Append newer messages
            this.messages = [...this.messages, ...newMessages];
        }
        
        // NOTE: We do NOT trim here during active loading
        // Trimming only happens during LRU eviction (when chat is closed/evicted)
        // This allows procedural scroll to load all messages as needed
        
        this.touch();
    }
    
    /**
     * Trim messages to the configured maximum (called during LRU eviction)
     * Keeps the most recent messages
     */
    trimToMax() {
        if (this.messages.length > MESSAGE_CACHE_CONFIG.maxMessagesPerChat) {
            // Keep the most recent messages (at the end)
            const excess = this.messages.length - MESSAGE_CACHE_CONFIG.maxMessagesPerChat;
            this.messages = this.messages.slice(excess);
            // Adjust loadedOffset since we removed older messages
            this.loadedOffset = Math.max(0, this.loadedOffset - excess);
            this.isFullyLoaded = false; // We no longer have all messages
        }
    }
    
    /**
     * Add a single new message (real-time update)
     * @param {Object} message - The new message to add
     */
    addNewMessage(message) {
        // Check for duplicates
        if (this.messages.some(m => m.id === message.id)) {
            return false;
        }
        
        // Find correct position based on timestamp
        const insertIndex = this.messages.findIndex(m => m.at > message.at);
        if (insertIndex === -1) {
            // Newest message, add to end
            this.messages.push(message);
        } else {
            // Insert at correct position
            this.messages.splice(insertIndex, 0, message);
        }
        
        this.totalInDb++;
        this.touch();
        
        // Trim if needed
        if (this.messages.length > MESSAGE_CACHE_CONFIG.maxMessagesPerChat) {
            this.messages.shift();  // Remove oldest
            this.loadedOffset = Math.max(0, this.loadedOffset - 1);
        }
        
        return true;
    }
    
    /**
     * Get the number of messages currently in cache
     */
    get cachedCount() {
        return this.messages.length;
    }
    
    /**
     * Check if there are more messages to load from DB
     */
    get hasMoreMessages() {
        return !this.isFullyLoaded && this.loadedOffset < this.totalInDb;
    }
}

/**
 * LRU Message Cache
 * Manages message caching across multiple chats with automatic eviction
 */
class MessageCache {
    constructor() {
        // Map of chatId -> ChatCacheEntry
        // Using Map to maintain insertion order for LRU
        this.cache = new Map();
        
        // File hash index for deduplication (loaded once at init)
        this.fileHashIndex = new Map();
        this.fileHashIndexLoaded = false;
    }
    
    /**
     * Get or create a cache entry for a chat
     * @param {string} chatId - The chat identifier
     * @returns {ChatCacheEntry}
     */
    getOrCreateEntry(chatId) {
        if (!this.cache.has(chatId)) {
            this.cache.set(chatId, new ChatCacheEntry(chatId));
            this._evictIfNeeded();
        }
        
        const entry = this.cache.get(chatId);
        entry.touch();
        
        // Move to end of Map (most recently used)
        this.cache.delete(chatId);
        this.cache.set(chatId, entry);
        
        return entry;
    }
    
    /**
     * Check if a chat has cached messages
     * @param {string} chatId - The chat identifier
     * @returns {boolean}
     */
    has(chatId) {
        return this.cache.has(chatId);
    }
    
    /**
     * Get cached messages for a chat (if any)
     * @param {string} chatId - The chat identifier
     * @returns {Array|null}
     */
    getMessages(chatId) {
        const entry = this.cache.get(chatId);
        if (entry) {
            entry.touch();
            // Move to end (most recently used)
            this.cache.delete(chatId);
            this.cache.set(chatId, entry);
            return entry.messages;
        }
        return null;
    }
    
    /**
     * Load initial messages for a chat from the database
     * @param {string} chatId - The chat identifier
     * @param {number} count - Number of messages to load
     * @returns {Promise<Array>} - The loaded messages
     */
    async loadInitialMessages(chatId, count = MESSAGE_CACHE_CONFIG.messagesPerBatch) {
        const entry = this.getOrCreateEntry(chatId);
        
        try {
            // Always refresh total count from DB (it may have changed since cache was created)
            entry.totalInDb = await invoke('get_chat_message_count', { chatId });
            
            if (entry.totalInDb === 0) {
                entry.isFullyLoaded = true;
                return [];
            }
            
            // If we already have enough messages cached, return them
            if (entry.messages.length >= count) {
                return entry.messages;
            }
            
            // We need to load messages from DB
            // If we have some cached (e.g., from real-time updates), we need to merge carefully
            const cachedMessages = entry.messages;
            const cachedCount = cachedMessages.length;
            
            // Load the most recent messages from DB
            const messages = await invoke('get_chat_messages_paginated', {
                chatId,
                limit: count,
                offset: 0
            });
            
            // Merge: DB messages are authoritative, but add any cached messages not in DB result
            // (This handles the case where real-time messages arrived but aren't in DB yet)
            const dbMessageIds = new Set(messages.map(m => m.id));
            const newCachedMessages = cachedMessages.filter(m => !dbMessageIds.has(m.id));
            
            // Combine: DB messages + any truly new cached messages, sorted by timestamp
            entry.messages = [...messages, ...newCachedMessages].sort((a, b) => a.at - b.at);
            entry.loadedOffset = messages.length; // Track how many we loaded from DB
            entry.isFullyLoaded = messages.length >= entry.totalInDb;
            
            return entry.messages;
        } catch (error) {
            // Return whatever we have cached on error
            return entry.messages;
        }
    }
    
    /**
     * Load more (older) messages for a chat
     * @param {string} chatId - The chat identifier
     * @param {number} count - Number of additional messages to load
     * @returns {Promise<Array>} - The newly loaded messages (older ones)
     */
    async loadMoreMessages(chatId, count = MESSAGE_CACHE_CONFIG.messagesPerBatch) {
        const entry = this.cache.get(chatId);
        if (!entry) {
            return [];
        }
        
        if (entry.isFullyLoaded || !entry.hasMoreMessages) {
            return [];
        }
        
        try {
            const messages = await invoke('get_chat_messages_paginated', {
                chatId,
                limit: count,
                offset: entry.loadedOffset
            });
            
            if (messages.length === 0) {
                entry.isFullyLoaded = true;
                return [];
            }
            
            // Prepend older messages
            entry.addMessages(messages, true);
            entry.loadedOffset += messages.length;
            entry.isFullyLoaded = entry.loadedOffset >= entry.totalInDb;
            
            return messages;
        } catch (error) {
            return [];
        }
    }
    
    /**
     * Add a new real-time message to the cache
     * @param {string} chatId - The chat identifier
     * @param {Object} message - The new message
     * @returns {boolean} - Whether the message was added (false if duplicate)
     */
    addNewMessage(chatId, message) {
        const entry = this.getOrCreateEntry(chatId);
        return entry.addNewMessage(message);
    }
    
    /**
     * Update the total message count for a chat (e.g., after sync)
     * @param {string} chatId - The chat identifier
     * @param {number} count - The new total count
     */
    updateTotalCount(chatId, count) {
        const entry = this.cache.get(chatId);
        if (entry) {
            entry.totalInDb = count;
            entry.isFullyLoaded = entry.loadedOffset >= count;
        }
    }
    
    /**
     * Get cache statistics for a chat
     * @param {string} chatId - The chat identifier
     * @returns {Object|null}
     */
    getStats(chatId) {
        const entry = this.cache.get(chatId);
        if (!entry) return null;
        
        return {
            cachedCount: entry.cachedCount,
            totalInDb: entry.totalInDb,
            loadedOffset: entry.loadedOffset,
            isFullyLoaded: entry.isFullyLoaded,
            hasMoreMessages: entry.hasMoreMessages,
            lastAccess: entry.lastAccess
        };
    }
    
    /**
     * Evict least recently used chats if cache is full
     * @private
     */
    _evictIfNeeded() {
        while (this.cache.size > MESSAGE_CACHE_CONFIG.maxCachedChats) {
            // Get the first entry (oldest/least recently used)
            const oldestKey = this.cache.keys().next().value;
            const oldestEntry = this.cache.get(oldestKey);
            
            // Keep only the last message for preview
            if (oldestEntry.messages.length > MESSAGE_CACHE_CONFIG.minMessagesForPreview) {
                oldestEntry.messages = oldestEntry.messages.slice(-MESSAGE_CACHE_CONFIG.minMessagesForPreview);
                oldestEntry.loadedOffset = MESSAGE_CACHE_CONFIG.minMessagesForPreview;
                oldestEntry.isFullyLoaded = false;
            }
            
            // Also evict from backend cache to keep them in sync
            invoke('evict_chat_messages', {
                chatId: oldestKey,
                keepCount: MESSAGE_CACHE_CONFIG.minMessagesForPreview
            }).catch(() => {});
            
            this.cache.delete(oldestKey);
        }
    }
    
    /**
     * Clear all cached messages (e.g., on logout)
     */
    clear() {
        this.cache.clear();
        this.fileHashIndex.clear();
        this.fileHashIndexLoaded = false;
    }
    
    /**
     * Clear cache for a specific chat
     * @param {string} chatId - The chat identifier
     */
    clearChat(chatId) {
        this.cache.delete(chatId);
    }
    
    /**
     * Trim a chat's cached messages to the configured maximum
     * Called when closing a chat to free memory while keeping recent messages
     * @param {string} chatId - The chat identifier
     */
    trimChat(chatId) {
        const entry = this.cache.get(chatId);
        if (entry) {
            entry.trimToMax();
        }
    }
    
    /**
     * Load the file hash index for deduplication
     * This should be called once at app init
     * @returns {Promise<Map>}
     */
    async loadFileHashIndex() {
        if (this.fileHashIndexLoaded) {
            return this.fileHashIndex;
        }
        
        try {
            const index = await invoke('get_file_hash_index');
            this.fileHashIndex = new Map(Object.entries(index));
            this.fileHashIndexLoaded = true;
            return this.fileHashIndex;
        } catch (error) {
            return this.fileHashIndex;
        }
    }
    
    /**
     * Check if a file hash exists in the index
     * @param {string} hash - The SHA256 hash of the file
     * @returns {Object|null} - The attachment reference if found
     */
    getExistingAttachment(hash) {
        return this.fileHashIndex.get(hash) || null;
    }
    
    /**
     * Add a new file hash to the index (after upload)
     * @param {string} hash - The SHA256 hash
     * @param {Object} attachmentRef - The attachment reference data
     */
    addFileHash(hash, attachmentRef) {
        this.fileHashIndex.set(hash, attachmentRef);
    }
    
    /**
     * Get overall cache statistics
     * @returns {Object}
     */
    getOverallStats() {
        let totalMessages = 0;
        let totalChats = this.cache.size;
        
        for (const entry of this.cache.values()) {
            totalMessages += entry.cachedCount;
        }
        
        return {
            cachedChats: totalChats,
            maxChats: MESSAGE_CACHE_CONFIG.maxCachedChats,
            totalCachedMessages: totalMessages,
            fileHashCount: this.fileHashIndex.size
        };
    }
}

// Export a singleton instance
const messageCache = new MessageCache();

// Make it globally accessible
window.messageCache = messageCache;