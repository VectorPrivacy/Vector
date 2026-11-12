//! Whisper's languages as `Intl.DisplayNames` names them: in English, and in themselves.

/// (code, English, native); an empty native name means it reads the same or has none.
const NAMES: &[(&str, &str, &str)] = &[
    ("af", "Afrikaans", "Afrikaans"),
    ("am", "Amharic", "አማርኛ"),
    ("ar", "Arabic", "العربية"),
    ("as", "Assamese", "অসমীয়া"),
    ("az", "Azerbaijani", "Azərbaycan"),
    ("ba", "Bashkir", ""),
    ("be", "Belarusian", "Беларуская"),
    ("bg", "Bulgarian", "Български"),
    ("bn", "Bangla", "বাংলা"),
    ("bo", "Tibetan", "བོད་སྐད་"),
    ("br", "Breton", "Brezhoneg"),
    ("bs", "Bosnian", "Bosanski"),
    ("ca", "Catalan", "Català"),
    ("cs", "Czech", "Čeština"),
    ("cy", "Welsh", "Cymraeg"),
    ("da", "Danish", "Dansk"),
    ("de", "German", "Deutsch"),
    ("el", "Greek", "Ελληνικά"),
    ("en", "English", "English"),
    ("es", "Spanish", "Español"),
    ("et", "Estonian", "Eesti"),
    ("eu", "Basque", "Euskara"),
    ("fa", "Persian", "فارسی"),
    ("fi", "Finnish", "Suomi"),
    ("fo", "Faroese", "Føroyskt"),
    ("fr", "French", "Français"),
    ("gl", "Galician", "Galego"),
    ("gu", "Gujarati", "ગુજરાતી"),
    ("ha", "Hausa", "Hausa"),
    ("haw", "Hawaiian", "ʻŌlelo Hawaiʻi"),
    ("he", "Hebrew", "עברית"),
    ("hi", "Hindi", "हिन्दी"),
    ("hr", "Croatian", "Hrvatski"),
    ("ht", "Haitian Creole", ""),
    ("hu", "Hungarian", "Magyar"),
    ("hy", "Armenian", "Հայերեն"),
    ("id", "Indonesian", "Indonesia"),
    ("is", "Icelandic", "Íslenska"),
    ("it", "Italian", "Italiano"),
    ("ja", "Japanese", "日本語"),
    ("jw", "Javanese", "Jawa"),
    ("ka", "Georgian", "ქართული"),
    ("kk", "Kazakh", "Қазақ тілі"),
    ("km", "Khmer", "ខ្មែរ"),
    ("kn", "Kannada", "ಕನ್ನಡ"),
    ("ko", "Korean", "한국어"),
    ("la", "Latin", "Latina"),
    ("lb", "Luxembourgish", "Lëtzebuergesch"),
    ("ln", "Lingala", "Lingála"),
    ("lo", "Lao", "ລາວ"),
    ("lt", "Lithuanian", "Lietuvių"),
    ("lv", "Latvian", "Latviešu"),
    ("mg", "Malagasy", "Malagasy"),
    ("mi", "Māori", "Māori"),
    ("mk", "Macedonian", "Македонски"),
    ("ml", "Malayalam", "മലയാളം"),
    ("mn", "Mongolian", "Монгол"),
    ("mr", "Marathi", "मराठी"),
    ("ms", "Malay", "Melayu"),
    ("mt", "Maltese", "Malti"),
    ("my", "Burmese", "မြန်မာ"),
    ("ne", "Nepali", "नेपाली"),
    ("nl", "Dutch", "Nederlands"),
    ("nn", "Norwegian Nynorsk", "Norsk nynorsk"),
    ("no", "Norwegian", "Norsk"),
    ("oc", "Occitan", "Occitan"),
    ("pa", "Punjabi", "ਪੰਜਾਬੀ"),
    ("pl", "Polish", "Polski"),
    ("ps", "Pashto", "پښتو"),
    ("pt", "Portuguese", "Português"),
    ("ro", "Romanian", "Română"),
    ("ru", "Russian", "Русский"),
    ("sa", "Sanskrit", "संस्कृत भाषा"),
    ("sd", "Sindhi", "سنڌي"),
    ("si", "Sinhala", "සිංහල"),
    ("sk", "Slovak", "Slovenčina"),
    ("sl", "Slovenian", "Slovenščina"),
    ("sn", "Shona", "ChiShona"),
    ("so", "Somali", "Soomaali"),
    ("sq", "Albanian", "Shqip"),
    ("sr", "Serbian", "Српски"),
    ("su", "Sundanese", "Basa Sunda"),
    ("sv", "Swedish", "Svenska"),
    ("sw", "Swahili", "Kiswahili"),
    ("ta", "Tamil", "தமிழ்"),
    ("te", "Telugu", "తెలుగు"),
    ("tg", "Tajik", "Тоҷикӣ"),
    ("th", "Thai", "ไทย"),
    ("tk", "Turkmen", "Türkmen dili"),
    ("tl", "Filipino", "Filipino"),
    ("tr", "Turkish", "Türkçe"),
    ("tt", "Tatar", "Татар"),
    ("uk", "Ukrainian", "Українська"),
    ("ur", "Urdu", "اردو"),
    ("uz", "Uzbek", "Oʻzbek"),
    ("vi", "Vietnamese", "Tiếng Việt"),
    ("yi", "Yiddish", "ייִדיש"),
    ("yo", "Yoruba", "Èdè Yorùbá"),
    ("yue", "Cantonese", "粵語"),
    ("zh", "Chinese", "中文"),
];

/// "Russian (Русский)", or just the English name where the native one reads the same.
pub fn name(code: &str) -> Option<String> {
    let (_, english, native) = NAMES.iter().find(|(c, _, _)| *c == code)?;
    Some(if native.is_empty() || native == english {
        english.to_string()
    } else {
        format!("{english} ({native})")
    })
}

/// A country code as its flag: two regional indicator letters.
pub fn flag(country: &str) -> Option<String> {
    let cc = country.trim();
    (cc.len() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic())).then(|| {
        cc.to_ascii_uppercase()
            .chars()
            .map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32)).unwrap())
            .collect()
    })
}
