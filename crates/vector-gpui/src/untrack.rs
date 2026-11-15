//! `cleanTrackingFromUrl`: tracking and marketing parameters taken out of links, in what is
//! sent and in what is shown, while "Prevent URL Tracking" is on. Referral credit stays: a
//! stable ID saying who gets credit is a choice the sharer made, a per-click ID is not.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `url` cleaned when the setting is on, else as it is.
pub fn shown(url: &str) -> String {
    if enabled() { clean(url) } else { url.to_string() }
}

/// Every http(s) URL in `text` cleaned, when the setting is on (`sendComposed`).
pub fn text(text: &str) -> String {
    static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)https?://[^\s<>"{}|\\^`\[\]]+"#).unwrap());
    if !enabled() {
        return text.to_string();
    }
    URL.replace_all(text, |c: &regex::Captures| clean(&c[0])).into_owned()
}

static REFERRAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:tag|partner|wmlspartner|campid|a(?:f|ff)id|affname|aff|aff_[a-z_]+|affiliate(?:_id)?|ref|refcode|ref_code|referral(?:_?code)?|invite(?:_?code)?|invited_by|via|promo(?:_?code)?|coupon|discount(?:_?code)?|sponsor|supporter|creator|epic_(?:affiliate|creator_id))$").unwrap()
});

static UTM_CLONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:utm_.|[a-z][a-z0-9]{0,3}_(?:source|medium|campaign|term|content|source_platform|creative_format|marketing_tactic)$)").unwrap()
});

static NAMESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:mtm_|pk_|hsa_|af_|adj_|adjust_|_branch_|bsft_|_sgm_|oly_|vero_|_x_(?:ads|ns|bg|sessn)_|cm_mmc|at_(?:campaign|channel|creation|custom|detail_placement|emailtype|format|general_placement|identifier|link|medium|network|platform|ptr_|recipient|send_|type$|variant)|mt_(?:click_id|network|adset|creative|sub\d|link_id)$|ebis(?:AdID|Other\d)$|nb_(?:adid|aname|asize|atype|cid|mtype|partner_name|partner_shared_id|pid|platform|product_sku|placement|expid_meta)$)").unwrap()
});

const GLOBAL: &[&str] = &[
    // Facebook/Meta
    "fbclid", "fbadid", "fb_action_ids", "fb_action_types", "fb_comment_id", "fb_ref", "fb_source",
    "action_object_map", "action_type_map", "action_ref_map", "mibextid", "extid",
    // Google Ads / Shopping / Analytics
    "gclid", "gclsrc", "dclid", "gbraid", "wbraid", "srsltid", "gad_campaignid", "_ga", "_gl", "usqp",
    // Microsoft/Bing
    "msclkid",
    // Yandex
    "yclid", "ysclid", "clckid", "_openstat", "yadclid", "yadordid", "ymclid", "ym_tracking_id",
    // Twitter/X: bare a/s/t stay in its own branch, globally they are search and timestamps.
    "twclid", "__twitter_impression", "tw_adid", "tw_kwdid", "tw_profile_id",
    // TikTok / Twitch
    "ttclid", "tt_medium", "tt_content",
    // Ad click IDs
    "rdt_cid", "epik", "li_fat_id", "tgclid", "line_uid", "yj_r", "pjclid",
    // Per-click IDs minted by affiliate networks; the affiliate's own ID is a referral.
    "irclickid", "irgwc", "ir_campaignid", "ir_adid", "ir_partnerid", "ircid", "impradid", "impradname",
    "cjevent", "cjdata", "awc", "sscid", "tduid", "ranMID", "ranEAID", "ranSiteID", "clickid", "iclid",
    "external_click_id", "rb_clickid", "wickedid", "sms_click", "sms_source", "sms_uph", "admitad_uid",
    "a8", "vc_lpp", "rtkcid", "x-clickref", "aiad_clid", "unicorn_click_id", "adsterra_clid",
    "adsterra_placement_id", "loclid", "ldtag_cl", "cstrackid", "oprtrack", "jmtyClId", "_zucks_suid",
    "famad_xuid", "bance_xuid", "asgtbndr", "winflncrtag", "vs_campaign_id", "erid", "mindbox-click-id",
    // Email and marketing automation
    "mc_cid", "mc_eid", "mc_tc", "ck_subscriber_id", "ml_subscriber", "ml_subscriber_hash", "ml_sub",
    "ml_sub_hash", "vero_conv", "vero_id", "mkt_tok", "__s", "elq", "elqTrackId", "elqaid", "elqat",
    "elqak", "elqCampaignId", "_kx", "_bhlid", "hootPostID", "eblastId", "ex_cid", "et_cid", "et_lid",
    "et_rid", "ss_campaign_id", "ss_campaign_name", "ss_campaign_sent_date", "sc_customer", "sc_eh",
    "sc_lid", "sc_llid", "sc_src", "sc_uid", "h_sid", "h_slt", "eml-name", "eml-mediaplan",
    "eml-publisher", "ems_dl", "emcs_t", "x-source", "x-a-medium", "mindbox-message-key",
    "user_email_address",
    // HubSpot
    "_hsenc", "_hsmi", "__hssc", "__hstc", "__hsfp", "hsCtaTracking",
    // Adobe / AT Internet / Webtrekk / Coremetrics / Piano / Matomo's old names
    "s_cid", "sc_cid", "adobe_mc_ref", "adobe_mc_sdid", "xtor", "wt_mc", "wt_zmc", "wtrid", "cm_me",
    "cm_cr", "cm_re", "cm_sp", "cx_click", "cx_recsOrder", "cx_recsWidget", "piwik_campaign", "piwik_kwd",
    // App-install and analytics session IDs
    "gps_adid", "is_retargeting", "analytics_context", "analytics_trace_id", "__io_lv", "_io_session_id",
    "_ly_c", "_ly_r", "_clde", "_cldee", "_bdadid", "adc_publisher", "adc_token", "bemobdata", "uzcid",
    "beyond_uzcvid", "beyond_uzmcvid", "vsm_type", "vsm_cid", "vsm_pid", "Tcsack", "srclt", "lt_r",
    "spot_im_redirect_source", "personaclick_search_query", "personaclick_input_query",
    "brave-campaign-id", "brave-creative-id", "brave-creative-set-id",
    // Publisher CMS campaign tags
    "cmpid", "ncid", "ftag", "os_ehash", "guccounter", "guce_referrer", "guce_referrer_sig",
    "tracking_source", "recommended_by",
    // Alibaba's pair
    "spm", "scm",
    // Generic tracking
    "referrer", "source", "campaign", "medium",
];

/// A query pair, its raw bytes kept for the rebuild and its decoded name for the rules.
struct Pair<'a> {
    raw: &'a str,
    name: String,
    keep: bool,
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    percent_decode(&s)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// `url` without its tracking parameters; the URL unchanged, byte for byte, when none are found
/// or it doesn't parse.
pub fn clean(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else { return url.to_string() };
    if !matches!(parsed.scheme(), "http" | "https") {
        return url.to_string();
    }
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    // The original split as written: [before ?][?query][#fragment].
    let (head, frag) = match url.find('#') {
        Some(i) => (&url[..i], Some(&url[i..])),
        None => (url, None),
    };
    let (base, query) = match head.find('?') {
        Some(i) => (&head[..i], Some(&head[i + 1..])),
        None => (head, None),
    };
    let mut pairs: Vec<Pair> = query
        .filter(|q| !q.is_empty())
        .map(|q| {
            q.split('&')
                .map(|raw| {
                    let name = raw.split_once('=').map_or(raw, |(n, _)| n);
                    Pair { raw, name: decode(name), keep: true }
                })
                .collect()
        })
        .unwrap_or_default();
    // The base split at its path, as written: the rules match the raw path, not a reparsed one.
    let path_at = base.find("://").map_or(0, |i| i + 3);
    let path_at = base[path_at..].find('/').map_or(base.len(), |i| path_at + i);
    let (origin, raw_path) = base.split_at(path_at);
    let mut path = raw_path.to_string();
    let mut frag_out = frag.map(str::to_string);

    let host_is = |domains: &[&str]| domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
    // `(^|\.)label\.[a-z]{2,}(\.[a-z]{2,})?$`: the label under a one- or two-part suffix.
    let label_is = |label: &str| {
        let parts: Vec<&str> = host.split('.').collect();
        (1..=2).any(|k| {
            parts.len() > k
                && parts[parts.len() - 1 - k] == label
                && parts[parts.len() - k..].iter().all(|p| p.len() >= 2 && p.bytes().all(|b| b.is_ascii_lowercase()))
        })
    };
    let drop = |pairs: &mut Vec<Pair>, names: &[&str]| {
        for p in pairs.iter_mut() {
            if names.contains(&p.name.as_str()) {
                p.keep = false;
            }
        }
    };
    let drop_matching = |pairs: &mut Vec<Pair>, re: &Regex, shield: Option<&Regex>| {
        for p in pairs.iter_mut() {
            if re.is_match(&p.name) && !shield.is_some_and(|s| s.is_match(&p.name)) {
                p.keep = false;
            }
        }
    };
    let re = |s: &str| Regex::new(s).unwrap();

    if host_is(&["youtube.com", "youtu.be", "youtube-nocookie.com"]) {
        drop(&mut pairs, &[
            "feature", "si", "is", "app", "kw", "annotation_id", "src_vid", "ab_channel", "start_radio",
            "rv", "pp", "themeRefresh", "source_ve_path", "embeds_referring_origin",
            "embeds_referring_euri", "embeds_euri", "embeds_origin", "embeds_loader_url_for_pings",
        ]);
    } else if label_is("google") {
        drop(&mut pairs, &[
            "ved", "ei", "sei", "oq", "aqs", "sourceid", "sxsrf", "rlz", "uact", "usg", "sa", "esrc", "cd",
            "cad", "atyp", "vet", "je", "dcr", "dpr", "iflsig", "fbs", "ictx", "cshid", "sclient", "_u",
            "site", "ie", "pcampaignid", "icid", "original_referer", "sstk", "spell",
        ]);
        drop_matching(&mut pairs, &re(r"^(?:gs_|gws_|gfe_|bi[hw]$|btn|sca_(?:esv|upv)$)"), None);
    } else if host_is(&["bing.com"]) {
        drop(&mut pairs, &[
            "cvid", "CVID", "qs", "sk", "sc", "sp", "pq", "form", "FORM", "PC", "ghsh", "ghacc", "ghpl",
            "toWww", "redig", "ntref", "ocid",
        ]);
    } else if host_is(&["duckduckgo.com"]) {
        drop(&mut pairs, &[
            "atb", "origin", "from", "vis", "perf_id", "vqd", "ia_source", "ad_provider", "ad_domain",
            "ad_type", "click_metadata", "parent_perf_id",
        ]);
        if pairs.iter().any(|p| p.keep && p.name == "q") {
            drop(&mut pairs, &["t"]);
        }
    } else if label_is("yandex") {
        drop(&mut pairs, &["lr", "redircnt", "clid", "banerid", "suggest_reqid", "did", "msid", "persistent_id", "from"]);
    } else if label_is("amazon") {
        if re(r"(?i)/dp/[A-Z0-9]+").is_match(&path) {
            // Only the product survives, and any referral credit (`tag`).
            for p in pairs.iter_mut() {
                p.keep = REFERRAL.is_match(&p.name);
            }
            if let Some(i) = path.find("/dp/") {
                path.truncate((i + 14).min(path.len()));
            }
        }
        drop(&mut pairs, &[
            "crid", "dib", "dib_tag", "keywords", "qid", "sprefix", "sr", "ie", "psc", "ref", "ref_",
            "linkCode", "creative", "creativeASIN", "ascsubtag", "asc_campaign", "asc_refurl",
            "asc_source", "content-id", "social_share", "th", "smid", "refRID", "rnid", "camp", "spIA",
            "qualifier", "_encoding", "dchild", "starsLeft", "skipTwisterOG", "aaxitk", "ms3_c", "colid",
            "coliid", "twchReferral", "ingress", "yTwchPos", "linkId", "geniuslink", "ufe",
        ]);
        drop_matching(&mut pairs, &re(r"^(?:p[fd]_rd_|__mk_|cv_ct_|sb-ci-|field-lbr_)"), None);
    } else if label_is("ebay") {
        if let Some(id) = re(r"/itm/(?:.+/)?(\d{6,})").captures(&path).map(|c| c[1].to_string()) {
            // `var` picks a variation and campaign credit is a referral; the rest goes.
            for p in pairs.iter_mut() {
                p.keep = p.name == "var" || REFERRAL.is_match(&p.name);
            }
            path = format!("/itm/{id}");
            frag_out = None;
        } else {
            drop(&mut pairs, &[
                "_trkparms", "_trksid", "itmprp", "itmmeta", "hash", "amdata", "epid", "_from", "mkcid",
                "mkrid", "toolid", "customid", "mkevt", "nordt", "rt", "ssspo", "sssrc", "ssuid",
                "widget_ver", "ssrc",
            ]);
        }
    } else if host_is(&["etsy.com"]) {
        drop(&mut pairs, &["click_key", "click_sum", "organic_search_click", "ref", "frs", "sts"]);
        drop_matching(&mut pairs, &re("^ga_"), None);
    } else if label_is("aliexpress") || label_is("lazada") || host_is(&["taobao.com", "tmall.com"]) {
        drop(&mut pairs, &[
            "algo_pvid", "algo_expid", "algo_exp_id", "curPageLogUid", "pdp_npi", "ws_ab_test", "btsid",
            "gps-id", "mall_affr", "terminal_id", "utparam", "utparam-url", "scm_id", "scm-url", "sk",
            "dp", "cv", "pvid", "ut_sk", "ali_refid", "ali_trackid", "acm", "abbucket", "abtest",
            "trackInfo", "impid", "clickTrackInfo", "ad_src", "impsrc",
        ]);
    } else if host_is(&["walmart.com"]) {
        drop(&mut pairs, &["u1", "from", "sourceid", "veh"]);
        drop_matching(&mut pairs, &re("^ath"), None);
    } else if host_is(&["target.com"]) {
        drop(&mut pairs, &["CPNG", "LID", "LNM", "DFA", "fndsrc", "adgroup", "network", "device", "location", "targetid", "ds_rl"]);
    } else if host_is(&["bestbuy.com"]) {
        drop(&mut pairs, &["acampID", "acampid", "mpid", "intl", "loc"]);
    } else if host_is(&["newegg.com"]) {
        drop(&mut pairs, &["ACRID", "ASUBID", "ASID", "nm_mc", "cm_mmc"]);
    } else if host_is(&["temu.com"]) {
        drop(&mut pairs, &["top_gallery_url", "adg_ctx", "mrk_rec", "freesia_scene", "share_img", "from_share", "spec_gallery_id"]);
        drop_matching(&mut pairs, &re(r"^(?:_x_|_oak_|_p_|_bg_|refer_)"), None);
    } else if host_is(&["twitter.com", "x.com", "t.co"]) {
        drop(&mut pairs, &["a", "s", "t", "cn", "cxt", "ref_src", "refsrc", "ref_url", "src"]);
    } else if host_is(&["facebook.com", "fb.com", "fb.me", "m.me", "messenger.com"]) {
        drop(&mut pairs, &[
            "sfnsn", "rdid", "paipv", "_rdr", "rdc", "rdr", "__tn__", "_nc_x", "comment_tracking", "dti",
            "eav", "idorvanity", "wtsid", "ls_ref", "action_history", "tracking", "referral_story_type",
            "video_source", "ftentidentifier", "pageid", "eid", "acontext", "external_ref",
            "gd_impression_id", "sale_post_id", "store_visit_source", "with_pv",
        ]);
        drop_matching(&mut pairs, &re(r"^(?:hc_|__cft__|__xts__)"), None);
    } else if host_is(&["instagram.com"]) {
        drop(&mut pairs, &["igshid", "igsh", "ig_rid", "stkn", "click_source"]);
    } else if host_is(&["threads.net", "threads.com"]) {
        drop(&mut pairs, &["xmt"]);
    } else if host_is(&["tiktok.com"]) {
        drop(&mut pairs, &[
            "is_from_webapp", "is_copy_url", "sender_device", "sender_web_id", "web_id", "refer", "u_code",
            "share_app_id", "share_app_name", "share_link_id", "share_item_id", "share_iid",
            "share_region", "social_share_type", "embed_source", "referer_url", "referer_video_id",
            "trackParams", "ug_btm", "enter_from", "preview_pb", "sec_user_id", "user_id", "_r", "_t", "_d",
            "sec_uid", "share_author_id", "tt_from", "enter_method",
        ]);
    } else if host_is(&["reddit.com", "redd.it"]) {
        drop(&mut pairs, &[
            "share_id", "correlation_id", "rdt", "ref", "ref_source", "ref_campaign", "entry_point",
            "target_user", "post_index", "post_fullname", "$deep_link", "$3p", "$original_url",
            "$android_deeplink_path",
        ]);
    } else if host_is(&["linkedin.com"]) {
        drop(&mut pairs, &[
            "trk", "trkInfo", "trackingId", "trackingCode", "refId", "referenceId", "originalSubdomain",
            "original_referer", "originalReferer", "midToken", "midSig", "eid", "courseClaim",
            "alternateChannel", "eBP", "mcid", "rcm", "isSS", "src",
        ]);
        drop_matching(&mut pairs, &re("^li[a-z]{2}$"), None);
    } else if host_is(&["pinterest.com", "pin.it"]) {
        drop(&mut pairs, &["nic", "nic_v1", "nic_v2", "amp_client_id", "mweb_unauth_id", "sender"]);
    } else if host_is(&["snapchat.com"]) {
        drop(&mut pairs, &["sc_referrer", "sc_ua", "sc_ref"]);
    } else if host_is(&["spotify.com"]) {
        drop(&mut pairs, &["si", "nd", "nid", "sp_cid", "dlsi", "pi", "referral"]);
    } else if host_is(&["apple.com"]) {
        drop(&mut pairs, &["itsct", "itscg", "cid", "ct", "pt", "app"]);
        drop_matching(&mut pairs, &re("^ign-itsc"), None);
    } else if host_is(&["twitch.tv"]) {
        drop(&mut pairs, &["tt_email_id"]);
    } else if host_is(&["steampowered.com", "steamcommunity.com"]) {
        drop(&mut pairs, &["snr", "curator_clanid"]);
    } else if host_is(&["gog.com"]) {
        drop(&mut pairs, &["pp", "track_click", "link_id"]);
    } else if host_is(&["epicgames.com"]) {
        drop(&mut pairs, &["epic_gameId"]);
    } else if host_is(&["humblebundle.com"]) {
        drop(&mut pairs, &["mcID", "linkID"]);
    } else if host_is(&["netflix.com"]) {
        drop(&mut pairs, &["trackId", "tctx"]);
    } else if host_is(&["imdb.com"]) {
        drop(&mut pairs, &["ref_"]);
        drop_matching(&mut pairs, &re("^pf_rd_"), None);
    } else if host_is(&["github.com"]) {
        drop(&mut pairs, &["email_token", "email_source", "notification_referrer_id", "reference_location"]);
    } else if host_is(&["vimeo.com"]) {
        drop(&mut pairs, &["share", "fl", "fe"]);
    } else if host_is(&["nytimes.com"]) {
        drop(&mut pairs, &["smid", "sgrp", "ugrp", "referringSource", "impression_id"]);
    } else if host_is(&["theguardian.com"]) {
        drop(&mut pairs, &["CMP"]);
    } else if host_is(&["bloomberg.com"]) {
        drop(&mut pairs, &["leadSource", "srnd", "sref"]);
    } else if host_is(&["substack.com"]) {
        drop(&mut pairs, &["triedRedirect", "isFreemail", "publication_id", "post_id"]);
    }

    // Every host: the exact names, then the pattern families, which spare referrals.
    drop(&mut pairs, GLOBAL);
    drop_matching(&mut pairs, &UTM_CLONE, Some(&REFERRAL));
    drop_matching(&mut pairs, &NAMESPACE, Some(&REFERRAL));

    let kept: Vec<&str> = pairs.iter().filter(|p| p.keep).map(|p| p.raw).collect();
    if kept.len() == pairs.len() && path == raw_path && frag_out.as_deref() == frag {
        return url.to_string();
    }
    let mut out = format!("{origin}{path}");
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(f) = frag_out {
        out.push_str(&f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn strips_trackers_and_keeps_the_rest() {
        assert_eq!(clean("https://example.com/a?utm_source=x&id=5&fbclid=abc"), "https://example.com/a?id=5");
        assert_eq!(clean("https://example.com/a?utm_source=x"), "https://example.com/a");
        assert_eq!(clean("https://example.com/a?id=5#frag"), "https://example.com/a?id=5#frag");
        assert_eq!(clean("https://example.com/a?gclid=1#frag"), "https://example.com/a#frag");
        assert_eq!(clean("https://example.com/"), "https://example.com/");
        assert_eq!(clean("not a url"), "not a url");
    }

    #[test]
    fn keeps_referrals_and_raw_bytes() {
        assert_eq!(clean("https://shop.example/x?ref=alice&utm_medium=y"), "https://shop.example/x?ref=alice");
        assert_eq!(clean("https://maps.google.com/x?data=!3m1!4b1&ved=2"), "https://maps.google.com/x?data=!3m1!4b1");
        assert_eq!(clean("https://example.com/s?q=a+b%20c&mc_eid=1"), "https://example.com/s?q=a+b%20c");
    }

    #[test]
    fn site_rules() {
        assert_eq!(clean("https://www.youtube.com/watch?v=abc&si=XYZ&t=42"), "https://www.youtube.com/watch?v=abc&t=42");
        assert_eq!(
            clean("https://www.amazon.com/Some-Thing/dp/B0ABCDEFGH/ref=sr_1?crid=1&tag=me-20&qid=9"),
            "https://www.amazon.com/Some-Thing/dp/B0ABCDEFGH?tag=me-20"
        );
        assert_eq!(clean("https://www.ebay.com/itm/Some-title/1234567890?_trkparms=x&var=7#a"), "https://www.ebay.com/itm/1234567890?var=7");
        assert_eq!(clean("https://x.com/u/status/1?s=20&t=abc"), "https://x.com/u/status/1");
        assert_eq!(clean("https://duckduckgo.com/?q=rust&t=h_"), "https://duckduckgo.com/?q=rust");
        assert_eq!(clean("https://www.notamazon.evil.com/dp/B0ABCDEFGH?ref=x"), "https://www.notamazon.evil.com/dp/B0ABCDEFGH?ref=x");
    }
}
