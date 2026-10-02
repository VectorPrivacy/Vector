// Community banners: each community's resolved image (null when it has none), and the
// communities whose banner this account hides, a list synced across its devices.
const s = $state({ src: {}, hidden: [] });

export function bannerState() { return s; }
export function setCommunityBanner(communityId, src) { s.src[communityId] = src || null; }
export function setHiddenBanners(ids) { s.hidden = ids || []; }
