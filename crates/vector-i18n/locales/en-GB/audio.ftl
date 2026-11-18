### The audio player in messages, and albums of songs.

audio-not-found = Audio file not found
audio-video-not-found = Video file not found
audio-model-downloading = Downloading model...
audio-play = Play
audio-pause = Pause
audio-previous = Previous
audio-next = Next
# The progress bar, for screen readers.
audio-seek = Seek
audio-lyrics = Lyrics
audio-lyrics-hide = Hide lyrics
audio-cover-art = View cover art

## An album

# $count is how many songs and $minutes how long they last, in whole minutes.
audio-album-stats = { $count ->
    [one] { $count } song · { $minutes } min
   *[other] { $count } songs · { $minutes } min
}
# $count is how many songs.
audio-album-songs = { $count ->
    [one] { $count } song
   *[other] { $count } songs
}
audio-shuffle = Shuffle
audio-repeat = Repeat
audio-repeat-one = Repeat one
audio-repeat-all = Repeat all
# $count is how many songs the album has.
audio-show-all = Show all { $count } songs
audio-show-fewer = Show fewer

## A video's controls

video-speed = Playback speed
video-mute = Mute
video-unmute = Unmute
video-volume = Volume
video-full-screen = Full screen
video-exit-full-screen = Exit full screen
