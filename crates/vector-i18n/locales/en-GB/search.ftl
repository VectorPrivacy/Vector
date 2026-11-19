### Message search: the search panel, its filters and its results.

search-placeholder = Search messages
# Shown in the field while a search is limited to the open chat. $chat is its name.
search-placeholder-in = Search { $chat }
search-indexing = Reading your messages…
search-none = No results
search-none-hint = Try fewer words, or take a filter away.
# $count is how many messages matched.
search-results = { $count ->
    [one] { $count } result
   *[other] { $count } results
}
# $shown of $count matches are listed.
search-results-capped = Showing the newest { $shown } of { $count } results
# $shown of $count matches are listed, the oldest first.
search-results-capped-oldest = Showing the oldest { $shown } of { $count } results
search-newest = Newest
search-oldest = Oldest
search-sort = Sort order
search-close = Close search
search-private = Searched on this device only. Nothing leaves it.
search-shortcut-hint = Up and Down to move, Enter to open, Esc to close

## Filters. The words before the colon in the search field (from:, in:, ...) stay English, as
## they do in every language; these are the labels shown beside them.

search-options = Search options
search-opt-from = From a person
search-opt-mentions = Mentions a person
search-opt-has = Has a link, image, video, audio or file
search-opt-in = In a chat or channel
search-opt-before = Before a date
search-opt-after = After a date
search-opt-during = On a date
search-has-link = Link
search-has-image = Image
search-has-video = Video
search-has-audio = Audio
search-has-file = File
search-me = Me
search-today = Today
search-yesterday = Yesterday
# How a date is typed into a before:, after: or during: filter.
search-date-hint = A date, like 2026-11-18, 2026-11 or 2026
search-remove-filter = Remove filter
search-you = You
search-in-chat = Search in Chat
# The header's search button. $keys is its shortcut, such as Ctrl+F.
search-button-tip = Search ({ $keys })
