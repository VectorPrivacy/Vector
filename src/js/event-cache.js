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

    // Maximum events to keep per conversation in cache. The buffer may hold a
    // (older) seek segment + the (newest) tail segment with a gap between them;
    // this caps their combined size before the seek segment is evicted.
    maxEventsPerConversation: 1000,

    // The newest N events are PINNED — never evicted, never dropped by a seek.
    // entry.events[last] is therefore always the true latest message, which the
    // chat-list preview/sort/status read by walking from the array's end.
    tailBudget: 500,

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
        // Non-congruent history: `events` may be [seek-region … GAP … tail-region].
        // gapAfterId = the id of the seek-region's NEWEST row, after which a gap
        // precedes the pinned tail. null = the buffer is one contiguous run (the
        // common case: fresh open, tail-only, or a gap that closed on scroll).
        this.gapAfterId = null;
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
     * Index of the first event in the PINNED tail segment. The tail is the newest
     * `tailBudget` events that are part of the contiguous run reaching the live tail.
     * When there's a gap, the tail starts AFTER the gap (everything below gapAfterId
     * in the array is the seek segment, which is droppable). When there's no gap, the
     * tail is simply the newest `tailBudget` of the whole array. Returns 0 when the
     * buffer is at or under the budget (the entire array is the tail).
     */
    _tailStartIndex() {
        const budget = EVENT_CACHE_CONFIG.tailBudget;
        if (this.gapAfterId != null) {
            const gapIdx = this.events.findIndex(e => e.id === this.gapAfterId);
            // Everything strictly after the gap boundary is the tail run. Keep all
            // of it (it is, by construction, the rows from the newest page onward).
            if (gapIdx !== -1) return gapIdx + 1;
            // gapAfterId no longer present (its row was the seek edge and got dropped)
            // — treat the buffer as contiguous and fall through to the budget rule.
        }
        return Math.max(0, this.events.length - budget);
    }

    /**
     * Trim events down toward the configured maximum WITHOUT ever dropping the tail.
     * The tail (newest `tailBudget`, or everything past the gap) is pinned; only the
     * seek segment (oldest rows below the gap, or the oldest of a contiguous over-cap
     * run) is shed. Called during LRU eviction / chat close.
     */
    trimToMax() {
        const max = EVENT_CACHE_CONFIG.maxEventsPerConversation;
        if (this.events.length <= max) return;

        // Never trim into the tail: the most we may drop is everything below the
        // tail-start index, capped so the kept array lands at the budget.
        const tailStart = this._tailStartIndex();
        const overBy = this.events.length - max;
        const drop = Math.min(overBy, tailStart);
        if (drop <= 0) return;

        this.events.splice(0, drop);
        // Dropping the seek segment removes the gap's older side; if the whole seek
        // segment is gone, the buffer is contiguous again.
        if (this.gapAfterId != null && !this.events.some(e => e.id === this.gapAfterId)) {
            this.gapAfterId = null;
        }
        // loadedOffset counts from the newest page; dropping OLDER rows below the tail
        // doesn't change how much of the tail we hold, but keep it a safe lower bound.
        this.loadedOffset = Math.max(0, this.loadedOffset - drop);
        this.isFullyLoaded = false; // we no longer hold the oldest rows

        // Rebuild the Set to match the trimmed events
        this.eventIds = new Set(this.events.map(e => e.id));
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

        // Segment-confined insert: when there's a gap the buffer is non-monotonic
        // ([seek … GAP … tail]), so a single binary search over the whole array could
        // land a mid-history arrival inside the WRONG segment and weld it across the
        // gap. Resolve which segment the event belongs to by its timestamp first.
        if (this.events.length === 0) {
            this.events.push(event);
        } else if (this.gapAfterId == null) {
            // Contiguous: ordinary ASC insert (fast-path append for the newest).
            this._insertSortedInRange(event, 0, this.events.length);
        } else {
            const gapIdx = this.events.findIndex(e => e.id === this.gapAfterId);
            if (gapIdx === -1) {
                // Boundary row gone (shouldn't happen) — treat as contiguous.
                this.gapAfterId = null;
                this._insertSortedInRange(event, 0, this.events.length);
            } else {
                const tailStart = gapIdx + 1;
                const tailOldestAt = this.events[tailStart]?.at ?? Infinity;
                const seekNewestAt = this.events[gapIdx].at;
                if (event.at >= tailOldestAt) {
                    // Belongs in the tail (incl. the common genuine-newest tail arrival).
                    this._insertSortedInRange(event, tailStart, this.events.length);
                } else if (event.at <= seekNewestAt) {
                    // Belongs in the seek region.
                    this._insertSortedInRange(event, 0, tailStart);
                } else {
                    // Falls strictly inside the unfetched gap interior. Inserting it into
                    // either segment would corrupt a boundary or falsely bridge the gap;
                    // it lives in the DB and the gap-fill scroll will pick it up. Drop it
                    // from the in-memory buffer (don't add to the Set) but still count it.
                    this.totalInDb++;
                    this.touch();
                    return false;
                }
            }
        }

        this.eventIds.add(event.id);
        this.totalInDb++;
        this.touch();

        // Trim if needed: drop the OLDEST row (the seek segment's oldest, or the
        // oldest of a contiguous over-cap run) — NEVER the tail. The cap (~1000)
        // sits well above the tail budget (~500), so a single overflow shift only
        // ever sheds a seek-region row.
        if (this.events.length > EVENT_CACHE_CONFIG.maxEventsPerConversation) {
            const removed = this.events.shift();
            this.eventIds.delete(removed.id);
            this.loadedOffset = Math.max(0, this.loadedOffset - 1);
            // If the shed row was the gap boundary, the whole seek region is gone now
            // (shift drops from the front) → the buffer is contiguous.
            if (this.gapAfterId === removed.id) this.gapAfterId = null;
        }

        return true;
    }

    /**
     * Insert `event` into the sorted sub-range [lo, hi) of this.events by .at,
     * keeping that sub-range ASC. Fast-paths an append at the range's tail.
     * @private
     */
    _insertSortedInRange(event, lo, hi) {
        // Append fast-path: at-or-after the range's current last element.
        if (hi <= lo || event.at >= this.events[hi - 1].at) {
            this.events.splice(hi, 0, event);
            return;
        }
        let low = lo, high = hi;
        while (low < high) {
            const mid = (low + high) >>> 1;
            if (this.events[mid].at < event.at) low = mid + 1;
            else high = mid;
        }
        this.events.splice(low, 0, event);
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
                // Return the cache's OWN array, not a fresh []. A community with no real messages
                // can still have system events (member joins) that get revealed/added into this array
                // afterward; callers alias `chat.messages` to it, and the live system-event handler
                // mutates it in place — a throwaway [] would orphan all of that (joins never render,
                // empty-state placeholder sticks).
                return entry.events;
            }

            // ALWAYS re-anchor to the DB's NEWEST page — a chat opens at the live tail. A prior
            // scroll-up or seek (jump/reply) may have left entry.events as a stale mid-history slice;
            // reusing it would reopen the chat mid-history with no newer rows to scroll down to.
            // Rebuild from the newest page, keeping only not-yet-persisted pending/failed sends (and
            // any realtime arrival within the page's time range), so the window opens CONTIGUOUS at
            // now. Older history re-loads anchored on scroll-up — O(window), no stale accretion.
            const cachedEvents = entry.events;
            const events = await invoke('get_message_views', {
                chatId: conversationId,
                limit: count,
                offset: 0
            });
            const dbEventIds = new Set(events.map(e => e.id));
            const pageOldestAt = events.length ? events.reduce((m, e) => Math.min(m, e.at), Infinity) : 0;
            const keepCached = cachedEvents.filter(e =>
                !dbEventIds.has(e.id) && (e.pending || e.failed || e.at >= pageOldestAt)
            );
            entry.events = [...events, ...keepCached].sort((a, b) => a.at - b.at);
            entry.loadedOffset = events.length;
            entry.isFullyLoaded = events.length >= entry.totalInDb;
            // Re-anchored to a clean newest page → the buffer is contiguous; any prior
            // seek region (older than this page) was dropped, so there's no gap.
            entry.gapAfterId = null;

            // Rebuild Set to match the re-anchored events (for O(1) duplicate checks)
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
     * Merge an anchored seek slice (jump-to-unread / reply-jump) into a conversation
     * WITHOUT dropping the pinned tail. Mutates the entry's events array IN PLACE so
     * aliases (chat.messages / getEventsRef) stay valid.
     *
     * The result is `[seek-region … (maybe GAP) … tail-region]`:
     *  - The tail (the contiguous run reaching events[last], i.e. the true latest) is
     *    ALWAYS preserved → chat-list preview/sort/status read the real newest message.
     *  - A PRIOR seek segment is discarded first (at most one seek + the tail).
     *  - The new slice merges in, deduped, sorted ASC by .at.
     *  - gapAfterId is set to the seek-region's newest id when the slice does NOT reach
     *    the tail (a real gap), or null when slice and tail overlap/touch (contiguous).
     *
     * @param {string} conversationId
     * @param {Array} slice - ASC-by-.at messages for the seek window
     * @returns {Array} - the entry's events array (now tail-pinned + seek-merged)
     */
    seedWindow(conversationId, slice) {
        const entry = this.getOrCreateEntry(conversationId);

        // Identify the tail run to preserve: everything past an existing gap, else the
        // newest tailBudget of the current (contiguous) buffer. This run reaches the
        // live tail and must survive the seed untouched.
        const budget = EVENT_CACHE_CONFIG.tailBudget;
        let tailStart;
        if (entry.gapAfterId != null) {
            const gi = entry.events.findIndex(e => e.id === entry.gapAfterId);
            tailStart = gi !== -1 ? gi + 1 : Math.max(0, entry.events.length - budget);
        } else {
            tailStart = Math.max(0, entry.events.length - budget);
        }
        const tail = entry.events.slice(tailStart);

        // Dedup the incoming slice against the tail (an overlap row is common when the
        // seek lands near the tail). Slice rows that aren't in the tail are the seek
        // region. Keep ASC order.
        const tailIds = new Set(tail.map(e => e.id));
        const sliceSorted = slice.slice().sort((a, b) => a.at - b.at);
        const seekRegion = sliceSorted.filter(e => !tailIds.has(e.id));

        // Contiguity test: the seek region touches/overlaps the tail when its newest
        // event is at-or-after the tail's oldest event. (When the slice fully overlapped
        // the tail, seekRegion is empty and it's trivially contiguous.)
        let gapAfterId = null;
        if (seekRegion.length && tail.length) {
            const seekNewestAt = seekRegion[seekRegion.length - 1].at;
            const tailOldestAt = tail[0].at;
            if (seekNewestAt < tailOldestAt) {
                gapAfterId = seekRegion[seekRegion.length - 1].id;
            }
        }

        // Rebuild events in place: seek region (older) then tail (newer). When there's
        // no gap, this is one contiguous ASC run; when there is, the boundary is gapAfterId.
        // Re-sort defensively so a touching (non-gap) merge stays globally ASC.
        const merged = gapAfterId != null
            ? [...seekRegion, ...tail]
            : [...seekRegion, ...tail].sort((a, b) => a.at - b.at);
        entry.events.length = 0;
        for (const e of merged) entry.events.push(e);
        entry.eventIds = new Set(entry.events.map(e => e.id));
        entry.gapAfterId = gapAfterId;

        // Seek slice is not the whole chat → allow further loading. loadedOffset is
        // meaningless as a from-newest count once non-congruent, so park it and keep
        // totalInDb a safe lower bound.
        entry.loadedOffset = entry.events.length;
        entry.totalInDb = Math.max(entry.totalInDb, entry.events.length);
        entry.isFullyLoaded = false;
        entry.touch();
        return entry.events;
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
     * Merge a batch of events into a conversation, deduping by id and keeping the
     * array IN PLACE (chat.messages aliases survive). Used by the anchored window
     * extends: the anchored DB read overlaps the window edge by one row (the
     * `<= anchor` include / `> anchor` exclude boundary), so the overlap row is
     * dropped here rather than rendered twice. Resulting array stays ASC by .at.
     *
     * Gap-aware: when there's a tracked gap (seek region below the tail) an
     * append (prepend=false) inserts the newer rows AT THE GAP BOUNDARY — between
     * the seek region and the tail — NOT at the array end (which would weld them
     * onto the tail and skip the still-unfetched gap interior). The gap boundary
     * advances to the newest inserted row; when those rows reach the tail's oldest
     * (timestamps touch/overlap) the gap CLOSES (gapAfterId = null, one contiguous
     * run). A prepend grows the seek region's older side and leaves the gap intact.
     *
     * @param {string} conversationId
     * @param {Array} newEvents - ASC-by-.at batch (older for prepend, newer for append)
     * @param {boolean} prepend - true = older rows above, false = newer rows below
     * @returns {number} count of genuinely-new rows merged
     */
    addEvents(conversationId, newEvents, prepend = true) {
        if (!newEvents || !newEvents.length) return 0;
        const entry = this.getOrCreateEntry(conversationId);
        const fresh = newEvents.filter(e => !entry.eventIds.has(e.id));
        if (!fresh.length) return 0;
        for (const e of fresh) entry.eventIds.add(e.id);
        fresh.sort((a, b) => a.at - b.at);

        if (prepend) {
            // Older rows go to the front (grow the seek region / the head). The gap
            // (if any) sits further down and is unaffected.
            entry.events.unshift(...fresh);
        } else if (entry.gapAfterId != null) {
            // Newer rows fill the gap between the seek region and the tail. Insert
            // them right after the gap boundary, then advance/close the gap.
            const gapIdx = entry.events.findIndex(e => e.id === entry.gapAfterId);
            const insertAt = gapIdx === -1 ? entry.events.length : gapIdx + 1;
            entry.events.splice(insertAt, 0, ...fresh);

            // Gap closes when the fetched rows definitively overlap the tail: the newest
            // fetched row is STRICTLY past the next (tail) row, leaving no interior. The
            // strict `>` avoids a same-second false-close that could weld an unfetched
            // equal-timestamp interior row. Otherwise the boundary advances to the newest
            // fetched row; the caller force-closes (closeGap) when its batch came back
            // short (DB interior exhausted → next rows are the tail).
            const newGapId = fresh[fresh.length - 1].id;
            const newGapIdx = insertAt + fresh.length - 1;
            const tailNext = entry.events[newGapIdx + 1];
            const reachedTail = tailNext && fresh[fresh.length - 1].at > tailNext.at;
            entry.gapAfterId = (reachedTail || newGapIdx + 1 >= entry.events.length)
                ? null
                : newGapId;
        } else {
            // Contiguous buffer: newer rows extend the tail at the end.
            entry.events.push(...fresh);
        }
        entry.totalInDb = Math.max(entry.totalInDb, entry.events.length);
        entry.touch();
        return fresh.length;
    }

    /**
     * Reconcile a pending→real id swap (on_sent finalize) so the dedup Set drops the
     * stale id and learns the real one. Without this the relay echo of the real id
     * slips past the O(1) dedup and renders as a duplicate row.
     * @param {string} conversationId
     * @param {string} oldId - the id being replaced (e.g. a pending id)
     * @param {string} newId - the final event id
     */
    replaceId(conversationId, oldId, newId) {
        const entry = this.cache.get(conversationId);
        if (!entry) return;
        entry.eventIds.delete(oldId);
        entry.eventIds.add(newId);
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
     * Get the underlying events array reference for a conversation. Used by
     * callers that want to detect whether their `chat.messages` is the same
     * array (post-openChat aliases the two; addEvent then mutates both).
     */
    getEventsRef(conversationId) {
        return this.cache.get(conversationId)?.events ?? null;
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
            lastAccess: entry.lastAccess,
            gapAfterId: entry.gapAfterId
        };
    }

    /**
     * The id after which a gap precedes the pinned tail (non-congruent history),
     * or null when the buffer is one contiguous run. Read by the gap-aware window
     * extends to decide in-memory vs anchored fetch at the seek/tail boundary.
     * @param {string} conversationId
     * @returns {string|null}
     */
    getGapAfterId(conversationId) {
        return this.cache.get(conversationId)?.gapAfterId ?? null;
    }

    /**
     * Force the buffer contiguous (clear the tracked gap) WITHOUT moving rows. Used
     * when the DB has nothing between the seek region and the tail (the rows below
     * the gap are unpersisted tail sends), so in-segment scrolling can reach them.
     * @param {string} conversationId
     */
    closeGap(conversationId) {
        const entry = this.cache.get(conversationId);
        if (entry) entry.gapAfterId = null;
    }

    /**
     * Drop the seek segment (everything up to and including the gap boundary),
     * keeping ONLY the tail, and clear the gap — in place (alias-safe). Used when
     * the window has moved into the tail and the older seek region is stale: a
     * scroll-up from the tail re-anchors cleanly rather than crossing the gap.
     * @param {string} conversationId
     */
    dropSeekSegment(conversationId) {
        const entry = this.cache.get(conversationId);
        if (!entry || entry.gapAfterId == null) return;
        const gapIdx = entry.events.findIndex(e => e.id === entry.gapAfterId);
        if (gapIdx !== -1) {
            const removed = entry.events.splice(0, gapIdx + 1);
            for (const e of removed) entry.eventIds.delete(e.id);
        }
        entry.gapAfterId = null;
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

            // Keep only the last event(s) for preview. slice(-N) keeps the NEWEST
            // rows = the tail, so the chat-list preview stays correct, and the
            // dropped seek region (if any) means the buffer is contiguous again.
            if (oldestEntry.events.length > EVENT_CACHE_CONFIG.minEventsForPreview) {
                oldestEntry.events = oldestEntry.events.slice(-EVENT_CACHE_CONFIG.minEventsForPreview);
                oldestEntry.loadedOffset = EVENT_CACHE_CONFIG.minEventsForPreview;
                oldestEntry.isFullyLoaded = false;
                oldestEntry.gapAfterId = null;
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
            totalCachedEvents: totalEvents
        };
    }
}

// Export a singleton instance
const eventCache = new EventCache();

// Make it globally accessible
window.eventCache = eventCache;
