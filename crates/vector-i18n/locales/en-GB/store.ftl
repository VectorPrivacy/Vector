### The App Store: browsing, an app's page, and its permissions.

store-title = App Store

## What an app needs to be played, inside "Made for …" and "For …"

store-needs-keys = a keyboard
store-needs-pad = a controller
store-needs-touch = a touchscreen
store-needs-keys-pad = a keyboard or a controller
store-needs-keys-touch = a keyboard or a touchscreen
store-needs-pad-touch = a controller or a touchscreen
store-needs-all = a keyboard or a controller or a touchscreen
store-needs-none = a keyboard and mouse
# On an app's row. $needs is one of the phrases above, such as "a controller".
store-for = For { $needs }
# On an app's page. $needs is one of the phrases above, such as "a controller".
store-made-for = Made for { $needs }, which this device doesn't seem to have

## Controls

store-control-touch = Touch
store-control-keys = Keyboard & Mouse
store-control-pad = Controller

## Actions

store-play = Play
store-open = Open
store-launching = Launching...
store-opening = Opening...
store-update = Update
store-install = Install
store-update-ready = Update ready
store-see-all = See All
store-play-with-friends = Play with Friends
store-details = Details
store-edit-listing = Edit Listing
store-copy-link = Copy Link
store-refresh = Refresh
store-try-again = Try Again
# $name is the app's name.
store-uninstall = Uninstall { $name }

## When an app was last played

store-played-now = just now
# $count is how many minutes ago.
store-played-minutes = { $count }m ago
# $count is how many hours ago.
store-played-hours = { $count }h ago
# $count is how many days ago.
store-played-days = { $count }d ago
# $when is how long ago, such as "5m ago" or "3 Oct".
store-played = Played { $when }
store-not-played = Not played yet

## The sidebar and the phone's chips

store-discover = Discover
store-library = Library
store-updates = Updates
store-all-apps = All Apps
# The phone's chip for every app
store-all = All
store-categories = Categories
store-apps-tools = Apps & Tools
store-fewer = Fewer
# $count is how many more categories there are.
store-more = { $count } More
store-this-device = This Device
store-no-controls = No controls found
store-add-controller = Press a button on a controller to add it
store-fit-only = Only What Fits
store-fits = Fits This Device
store-search = Search
store-results = Results
# $count is how many apps.
store-app-count = { $count ->
    [one] { $count } app
   *[other] { $count } apps
}

## Discover

# Shown in capitals over the spotlight
store-spotlight-together = Play Together
# Shown in capitals over the spotlight
store-spotlight-new = New in the App Store
store-shelf-controller = Grab Your Controller
store-shelf-touch = Play on the Go
store-shelf-keys = Made for Keyboard & Mouse
store-shelf-recent = Jump Back In
store-shelf-together = Play Together
store-new-arrivals = New Arrivals
store-browse = Browse by Category

## Lists

# $count is how many apps have an update waiting.
store-updates-ready = { $count ->
    [one] { $count } update ready
   *[other] { $count } updates ready
}
store-updating = Updating
store-update-all = Update All
# $query is what was searched for.
store-no-results = No results for “{ $query }”
store-no-results-hint = Try a name, a maker, or a kind of game
store-library-empty = Nothing installed yet
store-library-empty-hint = Apps you install from the App Store live here
store-discover-apps = Discover Apps
store-up-to-date = You're up to date
store-up-to-date-hint = Every app you have is on its newest version
store-nothing-fits = Nothing here fits this device
store-nothing-fits-hint = These are made for other controls
# $count is how many apps the list leaves out.
store-unfit = { $count ->
    [one] { $count } more is made for other controls
   *[other] { $count } more are made for other controls
}
store-showing-unfit = Showing what may not fit this device
store-show = Show
store-hide = Hide
store-loading = Opening the App Store
store-failed = The App Store didn't load

## An app's page

store-unknown-maker = Unknown maker
store-version = Version
# $version is the version installed.
store-you-have = You have { $version }
store-size = Size
store-released = Released
store-players = Players
store-kind = Kind
store-multiplayer = Multiplayer
store-solo = Solo
store-app = App
store-game = Game
# Shown in capitals
store-plays-with = Plays with
store-about = About
store-whats-new = What's New
# $version is the app's version.
store-version-number = Version { $version }
store-publisher = Publisher
store-developer = Developer
store-category = Category
store-installed = Installed
store-source = Source
store-app-id = App ID
store-information = Information

## Permissions

store-permissions = Permissions
store-permissions-loading = Loading permissions…
# $name is the app's name.
store-permissions-hint = What { $name } may use. Turn any off at any time.
store-permissions-reset = Reset Permissions

## Around the store

store-search-placeholder = Search the App Store
store-refresh-failed = Couldn't refresh the App Store
store-link-copied = Copied App Link
store-installing = Installing
store-uninstalling = Uninstalling...
store-failed-action = Failed
# $name is the app's name.
store-uninstall-title = Uninstall { $name }?
store-uninstall-text = This will remove the app from your device. You can reinstall it anytime.
store-uninstall-confirm = Uninstall
store-uninstalled = App uninstalled
store-install-first = Install it first

## Playing with friends: picking a chat to send the app to

store-friends-search = Search chats
# Stands in for a community with no name
store-friends-community = Community
store-friends-dm = Direct message
# $name is the app's name.
store-friends-hint = Send { $name } to a chat, and it opens for you
store-friends-no-chats = No chats yet: start one to play together
store-friends-no-match = No chats match
store-friends-send = Send

## Asking for permissions before an app opens

store-permissions-none = No permissions available
store-permissions-failed = Failed to load permissions
# $name is the app's name.
store-permissions-ask = { $name } asks for access
store-permissions-ask-text = Turn on what you're happy to share. It may work with less, and you can change these later on its App Store page.
store-permissions-continue = Continue
