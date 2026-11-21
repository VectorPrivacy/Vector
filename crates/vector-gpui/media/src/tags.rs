//! An audio file's tags as the player shows them: `get_audio_metadata`, with the cover art
//! kept as bytes for a native image rather than a data URI.

use crate::audio_engine::AudioInput;

pub struct Meta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover: Option<Vec<u8>>,
    pub lyrics: Option<crate::lyrics::Lyrics>,
    pub year: Option<u16>,
    pub chapters: Vec<crate::chapters::Chapter>,
}

/// None when the file has nothing worth showing: no title, art, lyrics or chapters.
pub fn read(input: &AudioInput) -> Result<Option<Meta>, String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    let tagged = match input {
        AudioInput::File(path) => lofty::read_from_path(path),
        AudioInput::Held(_) => lofty::probe::Probe::new(input.open()?)
            .guess_file_type()
            .map_err(|e| format!("Failed to read tags: {e}"))?
            .read(),
    }
    .map_err(|e| format!("Failed to read tags: {e}"))?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };
    let title = tag.title().map(|s| s.to_string());
    let artist = tag.artist().map(|s| s.to_string());
    let album = tag.album().map(|s| s.to_string());
    let cover = tag.pictures().first().map(|p| p.data().to_vec());
    let id3 = (input.extension().as_deref() == Some("mp3"))
        .then(|| {
            use lofty::file::AudioFile;
            let mut file = input.open().ok()?;
            lofty::mpeg::MpegFile::read_from(&mut file, lofty::config::ParseOptions::new())
                .ok()?
                .id3v2()
                .cloned()
        })
        .flatten();
    let lyrics = crate::lyrics::read(&tagged, id3.as_ref());
    let year = tag.date().map(|d| d.year);
    let chapters = crate::chapters::read(input, &tagged, id3.as_ref());
    if title.is_none() && cover.is_none() && lyrics.is_none() && chapters.is_empty() {
        return Ok(None);
    }
    Ok(Some(Meta { title, artist, album, cover, lyrics, year, chapters }))
}
