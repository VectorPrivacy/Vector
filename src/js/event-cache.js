/**
 * Event Cache - LRU (Least Recently Used) Cache for Displayable Events
 *
 * This module provides efficient on-demand event loading with automatic
 * memory management through LRU eviction.
 *
 * Architecture:
 * - The backend stores ALL events in a flat `events` table
 * - The Rust/State layer transforms flat events into displayable objects
 *   (e.g., messages with reactions attached, edits applied, etc.)
 * - This cache stores those displayable objects without needing to understand
 *   their specific types - all events are treated equally
 *
 * Event Types (all cached the same way):
 * - Messages (kind 14, 15)
 * - PIVX Payments (kind 30078)
 * - File transfers, etc.
 *
 * LRU Cache Explanation:
 * - LRU = "Least Recently Used"
 * - When the cache is full and we need to add a new item, we evict the item
 *   that was accessed least recently (oldest access time)
 * - This ensures frequently accessed chats stay in memory while rarely
 *   accessed ones are automatically cleaned up
 *
 * Benefits:
 * - Reduces RAM usage by not keeping all events in memory
 * - Maintains fast access for recently opened chats
 * - Automatically manages memory without manual cleanup
 * - Event-type agnostic: new event types work without cache changes
 */

/**
 * Configuration for the event cache
 */
const EVENT_CACHE_CONFIG = {
    // Maximum number of conversations to keep full event history for
    maxCachedConversations: 5,

    // Maximum events to keep per conversation in cache
    maxEventsPerConversation: 100,

    // Number of events to load per batch (for pagination)
    eventsPerBatch: 20,

    // Minimum events to keep even after eviction (for preview)
    minEventsForPreview: 1,
};

/**
 * Cache entry for a single conversation's events
 */
class ConversationCacheEntry {
    constructor(conversationId) {
        this.conversationId = conversationId;
        this.events = [];
        this.eventIds = new Set();  // O(1) duplicate lookup (~8 bytes per reference)
        this.lastAccess = Date.now();
        this.totalInDb = 0;  // Total displayable event count in database
        this.loadedOffset = 0;  // How many events from the end we've loaded
        this.isFullyLoaded = false;  // Whether all events are loaded
    }

    /**
     * Update the last access time (for LRU tracking)
     */
    touch() {
        this.lastAccess = Date.now();
    }

    /**
     * Add events to the cache (prepend older events)
     * O(m) Set sync + O(n+m) spread - optimal for sorted batches from backend
     * @param {Array} newEvents - Events to add (should be older than existing)
     * @param {boolean} prepend - If true, add to beginning (older events)
     */
    addEvents(newEvents, prepend = true) {
        // Sync Set for future O(1) duplicate checks in addEvent()
        for (const e of newEvents) {
            this.eventIds.add(e.id);
        }

        if (prepend) {
            // Prepend older events
            this.events = [...newEvents, ...this.events];
        } else {
            // Append newer events
            this.events = [...this.events, ...newEvents];
        }

        // NOTE: We do NOT trim here during active loading
        // Trimming only happens during LRU eviction (when conversation is closed/evicted)
        // This allows procedural scroll to load all events as needed

        this.touch();
    }

    /**
     * Trim events to the configured maximum (called during LRU eviction)
     * Keeps the most recent events
     */
    trimToMax() {
        if (this.events.length > EVENT_CACHE_CONFIG.maxEventsPerConversation) {
            // Keep the most recent events (at the end)
            const excess = this.events.length - EVENT_CACHE_CONFIG.maxEventsPerConversation;
            this.events = this.events.slice(excess);
            // Adjust loadedOffset since we removed older events
            this.loadedOffset = Math.max(0, this.loadedOffset - excess);
            this.isFullyLoaded = false; // We no longer have all events

            // Rebuild the Set to match trimmed events
            this.eventIds = new Set(this.events.map(e => e.id));
        }
    }

    /**
     * Add a single new event (real-time update)
     * O(1) duplicate check, O(1) append for newest (common), O(log n + n) for out-of-order
     * All event types (messages, payments, etc.) use this same method
     * @param {Object} event - The new event to add
     */
    addEvent(event) {
        // O(1) duplicate check via Set
        if (this.eventIds.has(event.id)) {
            return false;
        }

        // Fast path: newest event (most common real-time case) - O(1) append
        if (this.events.length === 0 || event.at >= this.events[this.events.length - 1].at) {
            this.events.push(event);
        } else {
            // Out-of-order: binary search for position + splice
            const insertIndex = this._binarySearchInsertIndex(event.at);
            this.events.splice(insertIndex, 0, event);
        }

        this.eventIds.add(event.id);
        this.totalInDb++;
        this.touch();

        // Trim if needed (remove oldest, update Set)
        if (this.events.length > EVENT_CACHE_CONFIG.maxEventsPerConversation) {
            const removed = this.events.shift();
            this.eventIds.delete(removed.id);
            this.loadedOffset = Math.max(0, this.loadedOffset - 1);
        }

        return true;
    }

    /**
     * Binary search to find insertion index for a timestamp
     * Returns the index where an event with this timestamp should be inserted
     * @param {number} timestamp - The timestamp to find position for
     * @returns {number} - The insertion index
     * @private
     */
    _binarySearchInsertIndex(timestamp) {
        let low = 0;
        let high = this.events.length;

        while (low < high) {
            const mid = (low + high) >>> 1;  // Unsigned right shift for floor division
            if (this.events[mid].at < timestamp) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }

        return low;
    }

    /**
     * Get the number of events currently in cache
     */
    get cachedCount() {
        return this.events.length;
    }

    /**
     * Check if there are more events to load from DB
     */
    get hasMoreEvents() {
        return !this.isFullyLoaded && this.loadedOffset < this.totalInDb;
    }
}

/**
 * LRU Event Cache
 * Manages event caching across multiple conversations with automatic eviction
 */
class EventCache {
    constructor() {
        // Map of conversationId -> ConversationCacheEntry
        // Using Map to maintain insertion order for LRU
        this.cache = new Map();

        // File hash index for deduplication (loaded once at init)
        this.fileHashIndex = new Map();
        this.fileHashIndexLoaded = false;
    }

    /**
     * Get or create a cache entry for a conversation
     * @param {string} conversationId - The conversation identifier
     * @returns {ConversationCacheEntry}
     */
    getOrCreateEntry(conversationId) {
        if (!this.cache.has(conversationId)) {
            this.cache.set(conversationId, new ConversationCacheEntry(conversationId));
            this._evictIfNeeded();
        }

        const entry = this.cache.get(conversationId);
        entry.touch();

        // Move to end of Map (most recently used)
        this.cache.delete(conversationId);
        this.cache.set(conversationId, entry);

        return entry;
    }

    /**
     * Check if a conversation has cached events
     * @param {string} conversationId - The conversation identifier
     * @returns {boolean}
     */
    has(conversationId) {
        return this.cache.has(conversationId);
    }

    /**
     * Get cached events for a conversation (if any)
     * @param {string} conversationId - The conversation identifier
     * @returns {Array|null}
     */
    getEvents(conversationId) {
        const entry = this.cache.get(conversationId);
        if (entry) {
            entry.touch();
            // Move to end (most recently used)
            this.cache.delete(conversationId);
            this.cache.set(conversationId, entry);
            return entry.events;
        }
        return null;
    }

    /**
     * Load initial events for a conversation from the database
     * Uses the backend's materialized views that transform flat events into displayable objects.
     * @param {string} conversationId - The conversation identifier
     * @param {number} count - Number of events to load
     * @returns {Promise<Array>} - The loaded events
     */
    async loadInitialEvents(conversationId, count = EVENT_CACHE_CONFIG.eventsPerBatch) {
        const entry = this.getOrCreateEntry(conversationId);

        try {
            // Always refresh total count from DB (it may have changed since cache was created)
            entry.totalInDb = await invoke('get_chat_message_count', { chatId: conversationId });

            if (entry.totalInDb === 0) {
                entry.isFullyLoaded = true;
                return [];
            }

            // If we already have enough events cached, return them
            if (entry.events.length >= count) {
                return entry.events;
            }

            // We need to load events from DB
            // If we have some cached (e.g., from real-time updates), we need to merge carefully
            const cachedEvents = entry.events;

            // Load materialized event views (events with computed reactions, edits applied, etc.)
            const events = await invoke('get_message_views', {
                chatId: conversationId,
                limit: count,
                offset: 0
            });

            // Merge: DB events are authoritative, but add any cached events not in DB result
            // (This handles the case where real-time events arrived but aren't in DB yet)
            const dbEventIds = new Set(events.map(e => e.id));
            const newCachedEvents = cachedEvents.filter(e => !dbEventIds.has(e.id));

            // Combine: DB events + any truly new cached events, sorted by timestamp
            entry.events = [...events, ...newCachedEvents].sort((a, b) => a.at - b.at);
            entry.loadedOffset = events.length; // Track how many we loaded from DB
            entry.isFullyLoaded = events.length >= entry.totalInDb;

            // Rebuild Set to match merged events (for O(1) duplicate checks)
            entry.eventIds = new Set(entry.events.map(e => e.id));

            return entry.events;
        } catch (error) {
            // Return whatever we have cached on error
            return entry.events;
        }
    }

    /**
     * Load more (older) events for a conversation
     * Uses the backend's materialized views.
     * @param {string} conversationId - The conversation identifier
     * @param {number} count - Number of additional events to load
     * @returns {Promise<Array>} - The newly loaded events (older ones)
     */
    async loadMoreEvents(conversationId, count = EVENT_CACHE_CONFIG.eventsPerBatch) {
        const entry = this.cache.get(conversationId);
        if (!entry) {
            return [];
        }

        if (entry.isFullyLoaded || !entry.hasMoreEvents) {
            return [];
        }

        try {
            // Load materialized event views with offset
            const events = await invoke('get_message_views', {
                chatId: conversationId,
                limit: count,
                offset: entry.loadedOffset
            });

            if (events.length === 0) {
                entry.isFullyLoaded = true;
                return [];
            }

            // Prepend older events
            entry.addEvents(events, true);
            entry.loadedOffset += events.length;
            entry.isFullyLoaded = entry.loadedOffset >= entry.totalInDb;

            return events;
        } catch (error) {
            return [];
        }
    }

    /**
     * Add a new real-time event to the cache
     * All event types (messages, payments, etc.) use this same method
     * @param {string} conversationId - The conversation identifier
     * @param {Object} event - The new event
     * @returns {boolean} - Whether the event was added (false if duplicate)
     */
    addEvent(conversationId, event) {
        const entry = this.getOrCreateEntry(conversationId);
        return entry.addEvent(event);
    }

    /**
     * Add a reaction to a cached event
     * Used for real-time reaction updates.
     * @param {string} conversationId - The conversation identifier
     * @param {string} eventId - The ID of the event to add the reaction to
     * @param {Object} reaction - The reaction object { id, reference_id, author_id, emoji }
     * @returns {boolean} - Whether the reaction was added
     */
    addReactionToEvent(conversationId, eventId, reaction) {
        const entry = this.cache.get(conversationId);
        if (!entry) return false;

        const event = entry.events.find(e => e.id === eventId);
        if (!event) return false;

        // Initialize reactions array if not present
        if (!event.reactions) {
            event.reactions = [];
        }

        // Check for duplicate reaction
        if (event.reactions.some(r => r.id === reaction.id)) {
            return false;
        }

        event.reactions.push(reaction);
        return true;
    }

    /**
     * Get a specific event from the cache
     * @param {string} conversationId - The conversation identifier
     * @param {string} eventId - The event ID
     * @returns {Object|null} - The event or null if not found
     */
    getEvent(conversationId, eventId) {
        const entry = this.cache.get(conversationId);
        if (!entry) return null;
        return entry.events.find(e => e.id === eventId) || null;
    }

    /**
     * Update the total event count for a conversation (e.g., after sync)
     * @param {string} conversationId - The conversation identifier
     * @param {number} count - The new total count
     */
    updateTotalCount(conversationId, count) {
        const entry = this.cache.get(conversationId);
        if (entry) {
            entry.totalInDb = count;
            entry.isFullyLoaded = entry.loadedOffset >= count;
        }
    }

    /**
     * Get cache statistics for a conversation
     * @param {string} conversationId - The conversation identifier
     * @returns {Object|null}
     */
    getStats(conversationId) {
        const entry = this.cache.get(conversationId);
        if (!entry) return null;

        return {
            cachedCount: entry.cachedCount,
            totalInDb: entry.totalInDb,
            loadedOffset: entry.loadedOffset,
            isFullyLoaded: entry.isFullyLoaded,
            hasMoreEvents: entry.hasMoreEvents,
            lastAccess: entry.lastAccess
        };
    }

    /**
     * Evict least recently used conversations if cache is full
     * @private
     */
    _evictIfNeeded() {
        while (this.cache.size > EVENT_CACHE_CONFIG.maxCachedConversations) {
            // Get the first entry (oldest/least recently used)
            const oldestKey = this.cache.keys().next().value;
            const oldestEntry = this.cache.get(oldestKey);

            // Keep only the last event for preview
            if (oldestEntry.events.length > EVENT_CACHE_CONFIG.minEventsForPreview) {
                oldestEntry.events = oldestEntry.events.slice(-EVENT_CACHE_CONFIG.minEventsForPreview);
                oldestEntry.loadedOffset = EVENT_CACHE_CONFIG.minEventsForPreview;
                oldestEntry.isFullyLoaded = false;
                // Rebuild Set to match trimmed events
                oldestEntry.eventIds = new Set(oldestEntry.events.map(e => e.id));
            }

            // Also evict from backend cache to keep them in sync
            invoke('evict_chat_messages', {
                chatId: oldestKey,
                keepCount: EVENT_CACHE_CONFIG.minEventsForPreview
            }).catch(() => {});

            this.cache.delete(oldestKey);
        }
    }

    /**
     * Clear all cached events (e.g., on logout)
     */
    clear() {
        this.cache.clear();
        this.fileHashIndex.clear();
        this.fileHashIndexLoaded = false;
    }

    /**
     * Clear cache for a specific conversation
     * @param {string} conversationId - The conversation identifier
     */
    clearConversation(conversationId) {
        this.cache.delete(conversationId);
    }

    /**
     * Trim a conversation's cached events to the configured maximum
     * Called when closing a conversation to free memory while keeping recent events
     * @param {string} conversationId - The conversation identifier
     */
    trimConversation(conversationId) {
        const entry = this.cache.get(conversationId);
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
        let totalEvents = 0;
        let totalConversations = this.cache.size;

        for (const entry of this.cache.values()) {
            totalEvents += entry.cachedCount;
        }

        return {
            cachedConversations: totalConversations,
            maxConversations: EVENT_CACHE_CONFIG.maxCachedConversations,
            totalCachedEvents: totalEvents,
            fileHashCount: this.fileHashIndex.size
        };
    }
}

// Export a singleton instance
const eventCache = new EventCache();

// Make it globally accessible
window.eventCache = eventCache;
