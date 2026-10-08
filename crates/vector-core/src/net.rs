//! Network utilities — SSRF protection, HTTP client helpers.

use url::Url;

/// Reject URLs that resolve to private/loopback/link-local addresses (SSRF protection).
pub fn validate_url_not_private(url_str: &str) -> Result<(), &'static str> {
    let parsed = Url::parse(url_str).map_err(|_| "Invalid URL")?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err("Only HTTP(S) URLs are allowed"),
    }

    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            let o = ip.octets();
            if ip.is_loopback() || ip.is_private() || ip.is_link_local()
                || ip.is_broadcast() || ip.is_unspecified()
                || (o[0] == 100 && o[1] >= 64 && o[1] <= 127)
            {
                return Err("Private/internal IP addresses are not allowed");
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if ip.is_loopback() || ip.is_unspecified() || is_ipv6_private(&ip) {
                return Err("Private/internal IP addresses are not allowed");
            }
        }
        Some(url::Host::Domain(domain)) => {
            if domain == "localhost" || domain.ends_with(".local") || domain.ends_with(".internal") {
                return Err("Local hostnames are not allowed");
            }
        }
        None => return Err("URL has no host"),
    }

    Ok(())
}

fn is_ipv6_private(ip: &std::net::Ipv6Addr) -> bool {
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return ipv4.is_loopback() || ipv4.is_private() || ipv4.is_link_local();
    }
    let segments = ip.segments();
    if segments[0] & 0xfe00 == 0xfc00 { return true; } // Unique local
    if segments[0] & 0xffc0 == 0xfe80 { return true; } // Link-local
    false
}

/// The exact `reqwest` these clients are built from.
///
/// Re-exported because call sites name its types (headers, bodies, methods): a consumer that
/// names its own `reqwest` version gets a different type with the same name.
pub use reqwest;

pub use crate::transport::Lane;

/// How long a transfer may go without moving a byte before it is given up on.
///
/// This is the only time limit a large transfer has. A total deadline caps the
/// file size a slow link can ever move (300 s at 1 Mbit/s is 36 MB), so
/// uploads and downloads are bounded by progress instead: any rate at all
/// keeps them alive, and only a dead connection is abandoned.
///
/// Two minutes rather than one because TCP's own retransmit backoff on a
/// lossy long-haul path reaches that between attempts: a shorter window
/// abandons a connection the kernel is still recovering. Servers we run keep
/// their gap timers above this, so the client is the one that gives up and
/// can say why, instead of meeting a socket the proxy already closed.
pub const TRANSFER_STALL: std::time::Duration = std::time::Duration::from_secs(120);

/// [`TRANSFER_STALL`] for the transport in use.
pub fn transfer_stall() -> std::time::Duration {
    crate::transport::budget(crate::transport::Op::TransferStall, TRANSFER_STALL)
}

/// What every request calls itself. Set once by the app with its own
/// version; until then, this crate's. Magnitude serves link previews to
/// Vector only, and judges that by this header, so it has to be present on
/// every request and has to start with `Vector/`. One string for every
/// user is also the least identifying choice: over Tor, a client that looks
/// like every other Vector looks like nothing in particular.
static USER_AGENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Name the app's version in the user agent. Call once at startup; a second
/// call is ignored.
pub fn set_app_version(version: &str) {
    let _ = USER_AGENT.set(format!("Vector/{version}"));
}

pub fn user_agent() -> String {
    USER_AGENT
        .get()
        .cloned()
        .unwrap_or_else(|| format!("Vector/{}", env!("CARGO_PKG_VERSION")))
}

/// An HTTP budget (a request total, a read) for the transport in use: a circuit to a fresh
/// host alone can take 45 s. Clearnet passes through.
pub fn tor_http_timeout(clearnet: std::time::Duration) -> std::time::Duration {
    crate::transport::budget(crate::transport::Op::HttpTotal, clearnet)
}

// ============================================================================
// HttpClient: egress decided per request
// ============================================================================
//
// A client is a descriptor (owner, lane, options), never a connection pool: `Req::send` asks
// the transport where this request may go, then runs it on the pooled reqwest client for that
// decision. A client taken before a network switch can therefore never send on the old egress,
// and a body in flight aborts when the network changes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ClientOptions {
    timeout: Option<std::time::Duration>,
    read_timeout: Option<std::time::Duration>,
    follow_redirects: bool,
    /// Never direct: a fetch decided while an anonymity network was chosen, that the user must
    /// never make from this device's own address.
    no_direct: bool,
}

#[derive(Clone, Debug)]
pub struct HttpClient {
    owner: u64,
    lane: Lane,
    opts: ClientOptions,
}

/// A client with the given total timeout, for content anyone can name.
///
/// Every fetch goes through the transport's decision for its own host: direct only while the
/// account is on Clearnet, through the bridge on an anonymity network, and refused in process
/// while that network is not up. Clippy forbids building a reqwest client anywhere else.
pub fn build_http_client(timeout: std::time::Duration) -> Result<HttpClient, String> {
    build_http_client_for(Lane::Shared, Some(timeout), None, true)
}

/// Like `build_http_client`, with the total timeout optional and
/// redirect-following switchable.
///
/// `timeout` is a deadline on the whole request; `None` for large transfers,
/// which are bounded by progress instead (see [`TRANSFER_STALL`]).
///
/// `read_timeout` resets on every byte of the response *body*. Before the
/// headers arrive it is a single deadline from the start of the request that
/// nothing resets — including bytes of a request body going out — so it must
/// stay `None` on an upload whose body may take longer than it.
///
/// Blossom PUT uses `follow_redirects = false`: a 3xx mid-upload would
/// re-issue as GET and drop the body, so the 3xx surfaces as the real status.
pub fn build_http_client_with_options(
    timeout: Option<std::time::Duration>,
    read_timeout: Option<std::time::Duration>,
    follow_redirects: bool,
) -> Result<HttpClient, String> {
    build_http_client_for(Lane::Shared, timeout, read_timeout, follow_redirects)
}

/// A client for `lane`: `Account` for requests that identify as the user (signed uploads).
pub fn build_http_client_for(
    lane: Lane,
    timeout: Option<std::time::Duration>,
    read_timeout: Option<std::time::Duration>,
    follow_redirects: bool,
) -> Result<HttpClient, String> {
    Ok(HttpClient {
        owner: crate::db::current_session_id(),
        lane,
        opts: ClientOptions { timeout, read_timeout, follow_redirects, no_direct: false },
    })
}

const DEFAULT_SHARED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A 30 s client for frequent small fetches (image cache, wallet polling).
pub fn shared_http_client() -> std::sync::Arc<HttpClient> {
    std::sync::Arc::new(HttpClient {
        owner: crate::db::current_session_id(),
        lane: Lane::Shared,
        opts: ClientOptions { timeout: Some(DEFAULT_SHARED_TIMEOUT), read_timeout: None, follow_redirects: true, no_direct: false },
    })
}

/// The pool key: one reqwest client per decision, so no pooled connection outlives its epoch.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PoolKey {
    epoch: u64,
    owner: u64,
    lane: Lane,
    opts: ClientOptions,
    direct: bool,
}

type Pools = std::collections::HashMap<PoolKey, reqwest::Client>;

static POOLS: std::sync::Mutex<Option<Pools>> = std::sync::Mutex::new(None);

/// Drop every pooled client: the next request builds one for the current egress.
pub fn forget_clients() {
    if let Ok(mut g) = POOLS.lock() {
        *g = None;
    }
}

/// Kept for callers that rebuilt the shared client on a Tor flip; clients are per request now.
pub fn rebuild_shared_http_client() -> Result<(), String> {
    forget_clients();
    Ok(())
}

/// Only builds requests: `Req::send` executes on the client the current egress gives.
#[allow(clippy::disallowed_methods)]
fn template() -> &'static reqwest::Client {
    static T: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    T.get_or_init(|| reqwest::Client::builder().build().expect("a plain reqwest client always builds"))
}

impl HttpClient {
    pub fn request<U: reqwest::IntoUrl>(&self, method: reqwest::Method, url: U) -> Req {
        Req { rb: template().request(method, url), client: self.clone() }
    }

    pub fn get<U: reqwest::IntoUrl>(&self, url: U) -> Req {
        self.request(reqwest::Method::GET, url)
    }

    pub fn post<U: reqwest::IntoUrl>(&self, url: U) -> Req {
        self.request(reqwest::Method::POST, url)
    }

    pub fn put<U: reqwest::IntoUrl>(&self, url: U) -> Req {
        self.request(reqwest::Method::PUT, url)
    }

    pub fn head<U: reqwest::IntoUrl>(&self, url: U) -> Req {
        self.request(reqwest::Method::HEAD, url)
    }

    pub fn delete<U: reqwest::IntoUrl>(&self, url: U) -> Req {
        self.request(reqwest::Method::DELETE, url)
    }

    pub fn lane(&self) -> Lane {
        self.lane
    }

    /// The session this client was built under.
    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// The same client speaking for `lane`: a request carrying the user's signed authorization
    /// identifies them, so it rides the Account lane.
    pub fn with_lane(&self, lane: Lane) -> HttpClient {
        HttpClient { lane, ..self.clone() }
    }

    /// The same client, refusing to go direct: for a fetch that was only allowed because the
    /// account was on an anonymity network, so a switch to Clearnet meanwhile fails it.
    pub fn without_direct(&self) -> HttpClient {
        HttpClient { opts: ClientOptions { no_direct: true, ..self.opts }, ..self.clone() }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::disallowed_methods)]
fn build_pooled(opts: ClientOptions, proxy: Option<String>) -> Result<reqwest::Client, String> {
    use crate::transport::{budget, Op};
    let mut builder = reqwest::Client::builder()
        // Kept connections: idle ones stay a while so a burst of small
        // fetches to one host (an edge, a Blossom server) rides one or a
        // few connections, and HTTP/2 multiplexes on them where offered.
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .http2_keep_alive_interval(std::time::Duration::from_secs(30))
        .http2_keep_alive_while_idle(true)
        // Magnitude's preview endpoint refuses a caller that does not say it is Vector.
        .user_agent(user_agent())
        // Bounded connect: a black-holed host (SYN swallowed, never refused) must
        // fail in seconds instead of silently consuming the whole request budget.
        .connect_timeout(budget(Op::HttpConnect, std::time::Duration::from_secs(15)));
    if let Some(t) = opts.timeout {
        builder = builder.timeout(budget(Op::HttpTotal, t));
    }
    if let Some(rt) = opts.read_timeout {
        builder = builder.read_timeout(budget(Op::HttpRead, rt));
    }
    if !opts.follow_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    } else {
        // Validate EVERY redirect hop, not just the initial URL: a public host answering
        // `302 Location: http://169.254.169.254/…` would otherwise walk past the SSRF check,
        // and one naming another network's host must not reach a resolver.
        builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects");
            }
            if let Some(dest) = attempt.url().host_str().and_then(crate::transport::Dest::parse) {
                let kind = crate::transport::preference().unwrap_or(crate::transport::Kind::Clearnet);
                if let Some(r) = crate::transport::route::pre_route(kind, &dest) {
                    return attempt.error(r.text());
                }
            }
            match validate_url_not_private(attempt.url().as_str()) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        }));
    }
    if let Some(url) = proxy {
        let proxy = reqwest::Proxy::all(url).map_err(|_| "Vector couldn't open its local proxy.".to_string())?;
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(|e| format!("Failed to build HTTP client: {}", e))
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::disallowed_methods)]
fn build_pooled(_opts: ClientOptions, _proxy: Option<String>) -> Result<reqwest::Client, String> {
    reqwest::Client::builder().build().map_err(|e| format!("HTTP client build failed: {e}"))
}

/// The pooled client for this decision, built and cached unless the epoch moved meanwhile.
fn pooled(desc: &HttpClient, epoch: u64, egress: &crate::transport::Egress) -> Result<reqwest::Client, HttpError> {
    let key = PoolKey {
        epoch,
        owner: desc.owner,
        lane: desc.lane,
        opts: desc.opts,
        direct: matches!(egress, crate::transport::Egress::Direct),
    };
    if let Some(c) = POOLS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&key).cloned())) {
        return Ok(c);
    }
    let proxy = match egress {
        #[cfg(not(target_arch = "wasm32"))]
        crate::transport::Egress::Proxy(t) => Some(crate::transport::bridge::proxy_url(t).map_err(HttpError::Refused)?),
        _ => None,
    };
    let client = build_pooled(desc.opts, proxy).map_err(|e| HttpError::Refused(crate::transport::ConnectError::Bridge(e)))?;
    if crate::transport::epoch() == epoch {
        if let Ok(mut g) = POOLS.lock() {
            g.get_or_insert_with(Default::default).entry(key).or_insert_with(|| client.clone());
        }
    }
    Ok(client)
}

/// A request being built. Forwards the builder methods call sites use.
pub struct Req {
    rb: reqwest::RequestBuilder,
    client: HttpClient,
}

impl Req {
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        <reqwest::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        reqwest::header::HeaderValue: TryFrom<V>,
        <reqwest::header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.rb = self.rb.header(key, value);
        self
    }

    pub fn headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        self.rb = self.rb.headers(headers);
        self
    }

    pub fn body<T: Into<reqwest::Body>>(mut self, body: T) -> Self {
        self.rb = self.rb.body(body);
        self
    }

    pub fn json<T: serde::Serialize + ?Sized>(mut self, json: &T) -> Self {
        self.rb = self.rb.json(json);
        self
    }

    pub fn query<T: serde::Serialize + ?Sized>(mut self, query: &T) -> Self {
        self.rb = self.rb.query(query);
        self
    }

    pub fn bearer_auth<T: std::fmt::Display>(mut self, token: T) -> Self {
        self.rb = self.rb.bearer_auth(token);
        self
    }

    pub fn basic_auth<U: std::fmt::Display, P: std::fmt::Display>(mut self, user: U, password: Option<P>) -> Self {
        self.rb = self.rb.basic_auth(user, password);
        self
    }

    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.rb = self.rb.timeout(timeout);
        self
    }

    /// The browser's own cache stays out of it (Vector Web).
    #[cfg(target_arch = "wasm32")]
    pub fn fetch_cache_no_store(mut self) -> Self {
        self.rb = self.rb.fetch_cache_no_store();
        self
    }

    /// Ask the transport where this request may go, then run it there. A refusal opens no
    /// socket and resolves nothing; a network change while it runs abandons it.
    #[allow(clippy::disallowed_methods)]
    pub async fn send(self) -> Result<Resp, HttpError> {
        let (_, req) = self.rb.build_split();
        let req = req.map_err(HttpError::from_reqwest)?;
        let host = req.url().host_str().unwrap_or_default().to_string();
        let port = req.url().port_or_known_default().unwrap_or(443);
        let desc = self.client;
        let (mut epoch, mut client) = (0, None);
        // A decision raced by a switch is taken again, at most three times; past that the
        // request runs on the last one and fails closed against its stale epoch.
        for _ in 0..3 {
            epoch = crate::transport::epoch();
            let egress = crate::transport::egress(desc.owner, desc.lane, &host, port);
            if let crate::transport::Egress::Refuse(e) = egress {
                return Err(HttpError::Refused(e));
            }
            if desc.opts.no_direct && egress == crate::transport::Egress::Direct {
                return Err(HttpError::Refused(crate::transport::ConnectError::Stale));
            }
            client = Some(pooled(&desc, epoch, &egress)?);
            if crate::transport::epoch() == epoch {
                break;
            }
        }
        let client = client.expect("the loop ran");
        // The change is polled first: a switch that lands before the first poll must stop the
        // request before it resolves or dials anything.
        let changed = crate::transport::changed(epoch);
        let run = client.execute(req);
        futures_util::pin_mut!(changed, run);
        match futures_util::future::select(changed, run).await {
            futures_util::future::Either::Left(_) => Err(HttpError::NetworkChanged),
            futures_util::future::Either::Right((Ok(inner), _)) => Ok(Resp { inner, epoch, owner: desc.owner }),
            futures_util::future::Either::Right((Err(e), _)) => Err(HttpError::from_reqwest_for(e, &host)),
        }
    }
}

/// A response body as a stream; `Send` natively, where the browser's isn't.
#[cfg(not(target_arch = "wasm32"))]
pub type BodyStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, HttpError>> + Send>>;
#[cfg(target_arch = "wasm32")]
pub type BodyStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, HttpError>>>>;

/// A response whose body reads stop when the network it came over changes.
pub struct Resp {
    inner: reqwest::Response,
    epoch: u64,
    owner: u64,
}

impl Resp {
    pub fn status(&self) -> reqwest::StatusCode {
        self.inner.status()
    }

    pub fn headers(&self) -> &reqwest::header::HeaderMap {
        self.inner.headers()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    pub fn url(&self) -> &reqwest::Url {
        self.inner.url()
    }

    pub fn error_for_status(self) -> Result<Resp, HttpError> {
        let (epoch, owner) = (self.epoch, self.owner);
        self.inner.error_for_status().map(|inner| Resp { inner, epoch, owner }).map_err(HttpError::from_reqwest)
    }

    fn still_current(&self) -> bool {
        crate::transport::epoch() == self.epoch && crate::db::live_session_id() == self.owner
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::disallowed_methods)]
    pub async fn chunk(&mut self) -> Result<Option<bytes::Bytes>, HttpError> {
        if !self.still_current() {
            return Err(HttpError::NetworkChanged);
        }
        let changed = crate::transport::changed(self.epoch);
        let read = self.inner.chunk();
        futures_util::pin_mut!(read, changed);
        match futures_util::future::select(read, changed).await {
            futures_util::future::Either::Left((r, _)) => r.map_err(HttpError::from_reqwest),
            futures_util::future::Either::Right(_) => Err(HttpError::NetworkChanged),
        }
    }

    #[allow(clippy::disallowed_methods)]
    pub fn bytes_stream(self) -> BodyStream {
        use futures_util::StreamExt;
        let (epoch, owner) = (self.epoch, self.owner);
        let inner = Box::pin(self.inner.bytes_stream());
        Box::pin(futures_util::stream::unfold((inner, false), move |(mut inner, done)| async move {
            if done {
                return None;
            }
            if crate::transport::epoch() != epoch || crate::db::live_session_id() != owner {
                return Some((Err(HttpError::NetworkChanged), (inner, true)));
            }
            let changed = crate::transport::changed(epoch);
            futures_util::pin_mut!(changed);
            match futures_util::future::select(inner.next(), changed).await {
                futures_util::future::Either::Left((Some(item), _)) => Some((item.map_err(HttpError::from_reqwest), (inner, false))),
                futures_util::future::Either::Left((None, _)) => None,
                futures_util::future::Either::Right(_) => Some((Err(HttpError::NetworkChanged), (inner, true))),
            }
        }))
    }

    async fn whole<T, F: std::future::Future<Output = Result<T, reqwest::Error>>>(epoch: u64, read: F) -> Result<T, HttpError> {
        let changed = crate::transport::changed(epoch);
        futures_util::pin_mut!(read, changed);
        match futures_util::future::select(read, changed).await {
            futures_util::future::Either::Left((r, _)) => r.map_err(HttpError::from_reqwest),
            futures_util::future::Either::Right(_) => Err(HttpError::NetworkChanged),
        }
    }

    pub async fn bytes(self) -> Result<bytes::Bytes, HttpError> {
        if !self.still_current() {
            return Err(HttpError::NetworkChanged);
        }
        Self::whole(self.epoch, self.inner.bytes()).await
    }

    pub async fn text(self) -> Result<String, HttpError> {
        if !self.still_current() {
            return Err(HttpError::NetworkChanged);
        }
        Self::whole(self.epoch, self.inner.text()).await
    }

    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, HttpError> {
        if !self.still_current() {
            return Err(HttpError::NetworkChanged);
        }
        Self::whole(self.epoch, self.inner.json::<T>()).await
    }
}

#[derive(Debug)]
pub enum HttpError {
    /// The transport refused before any socket opened.
    Refused(crate::transport::ConnectError),
    /// The network this request was on changed under it.
    NetworkChanged,
    Http { err: reqwest::Error, text: String },
}

impl HttpError {
    fn from_reqwest(err: reqwest::Error) -> Self {
        HttpError::Http { text: err.to_string(), err }
    }

    /// reqwest's text, unless the transport recorded why it refused this host just now.
    fn from_reqwest_for(err: reqwest::Error, host: &str) -> Self {
        let recorded = crate::transport::host::active().and_then(|a| {
            let (at, e) = a.last_failure(host)?;
            let fresh = at.elapsed() < std::time::Duration::from_secs(30);
            // Under Tor an arti failure keeps reqwest's own text, as it always read.
            let keep_reqwest = a.kind == crate::transport::Kind::Tor && matches!(e, crate::transport::ConnectError::Unreachable(_));
            (fresh && !keep_reqwest).then(|| e.text())
        });
        HttpError::Http { text: recorded.unwrap_or_else(|| err.to_string()), err }
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self, HttpError::Http { err, .. } if err.is_timeout())
    }

    pub fn is_connect(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            matches!(self, HttpError::Http { err, .. } if err.is_connect())
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    pub fn is_request(&self) -> bool {
        matches!(self, HttpError::Http { err, .. } if err.is_request())
    }

    pub fn status(&self) -> Option<reqwest::StatusCode> {
        match self {
            HttpError::Http { err, .. } => err.status(),
            _ => None,
        }
    }

    /// The network will come back: a switch, or the chosen kind still connecting. A policy
    /// refusal won't.
    pub fn is_transient(&self) -> bool {
        match self {
            HttpError::NetworkChanged => true,
            HttpError::Refused(e) => e.is_transient(),
            HttpError::Http { .. } => false,
        }
    }

    /// A refusal by the transport's rules (I2P-Only, another network's name), which no retry fixes.
    pub fn is_policy_refusal(&self) -> bool {
        matches!(self, HttpError::Refused(crate::transport::ConnectError::Refused(_)))
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Refused(e) => f.write_str(&e.text()),
            HttpError::NetworkChanged => f.write_str("The network changed."),
            HttpError::Http { text, .. } => f.write_str(text),
        }
    }
}

impl std::error::Error for HttpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HttpError::Http { err, .. } => Some(err),
            _ => None,
        }
    }
}

/// Upload bodies: ends the stream with an error when the epoch or live session changes.
pub fn guard_body<S, E>(
    body: S,
    epoch: u64,
    owner: u64,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    crate::transport::guard::guard_body(body, epoch, owner)
}

/// Hold a transfer that hit a network change until the network is back, then let it resume.
/// Policy refusals and other failures return at once with their text.
pub async fn wait_after(e: &HttpError) -> Result<(), String> {
    if !e.is_transient() {
        return Err(e.to_string());
    }
    crate::transport::wait_ready(crate::transport::budget(crate::transport::Op::Startup, std::time::Duration::ZERO)).await
}

/// Find the byte index where a bracket/paren group opened at `start` closes,
/// tracking nesting depth and honoring backslash escapes — markdown balances
/// both, so a naive first-closer scan desyncs on `[[claim]](evil)` or
/// `[claim\]](evil)` and lets the claim reach the URL scan. All compared
/// bytes are ASCII, so the returned index is char-boundary-safe.
fn md_group_close(bytes: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 1usize;
    let mut escaped = false;
    let mut j = start;
    loop {
        match bytes.get(j).copied() {
            None => return None,
            Some(b'\\') if !escaped => escaped = true,
            Some(b) if b == open && !escaped => depth += 1,
            Some(b) if b == close && !escaped => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => escaped = false,
        }
        j += 1;
    }
}

/// Rewrite markdown links so a preview-URL scan sees only real DESTINATIONS:
/// `[text](href)` keeps the href and drops the display text — a URL claimed in
/// the text must never win the OG preview over where the link actually goes —
/// `[text](<href>)` drops entirely (angle brackets are the no-preview syntax),
/// and `[text][ref]` drops the label (its destination is a definition scanned
/// on its own elsewhere in the text). Images (`![alt](url)`) render as literal
/// text in chat, so they pass through untouched.
pub fn strip_md_link_claims(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' || (i > 0 && bytes[i - 1] == b'!') {
            i += 1;
            continue;
        }
        let Some(close) = md_group_close(bytes, i + 1, b'[', b']') else { break };
        match bytes.get(close + 1).copied() {
            // Inline link: drop the label, contribute the destination.
            Some(b'(') => {
                let Some(paren) = md_group_close(bytes, close + 2, b'(', b')') else {
                    i = close + 1;
                    continue;
                };
                out.push_str(&text[last..i]);
                let href = text[close + 2..paren].trim();
                if !(href.starts_with('<') && href.ends_with('>')) {
                    out.push(' ');
                    out.push_str(href);
                    out.push(' ');
                }
                i = paren + 1;
                last = i;
            }
            // Reference link: drop the label; the `[ref]: url` definition line
            // carries the real destination and gets scanned as plain text.
            Some(b'[') => {
                let Some(ref_close) = md_group_close(bytes, close + 2, b'[', b']') else {
                    i = close + 1;
                    continue;
                };
                out.push_str(&text[last..i]);
                i = ref_close + 1;
                last = i;
            }
            _ => {
                i = close + 1;
            }
        }
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // strip_md_link_claims — preview scan must see destinations, not claims
    // ========================================================================

    #[test]
    fn md_link_claim_text_dropped_href_kept() {
        // The spoof shape: claimed URL first in raw text, real destination second.
        let out = strip_md_link_claims("[https://your-bank.com](https://evil.io)");
        assert!(!out.contains("your-bank.com"), "claimed text must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"), "real destination must reach the scan: {out}");
    }

    #[test]
    fn md_no_preview_link_dropped_entirely() {
        let out = strip_md_link_claims("see [docs](<https://vector.app/docs>) ok");
        assert!(!out.contains("vector.app"), "no-preview href must not reach the scan: {out}");
        assert!(out.contains("see ") && out.contains(" ok"));
    }

    #[test]
    fn md_image_passes_through() {
        let text = "![shot](https://host.io/img.png)";
        assert_eq!(strip_md_link_claims(text), text);
    }

    #[test]
    fn plain_text_and_bare_urls_untouched() {
        let text = "check https://vector.app and [also] (spaced) brackets";
        assert_eq!(strip_md_link_claims(text), text);
    }

    #[test]
    fn multiple_links_keep_document_order() {
        let out = strip_md_link_claims("[a](https://one.io) mid [b](https://two.io)");
        let one = out.find("https://one.io").expect("first href kept");
        let two = out.find("https://two.io").expect("second href kept");
        assert!(one < two);
    }

    #[test]
    fn nested_bracket_label_still_drops_claim() {
        let out = strip_md_link_claims("[[https://trusted.com]](https://evil.io)");
        assert!(!out.contains("trusted.com"), "nested-bracket claim must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"));
    }

    #[test]
    fn escaped_bracket_label_still_drops_claim() {
        let out = strip_md_link_claims(r"[https://trusted.com\]](https://evil.io)");
        assert!(!out.contains("trusted.com"), "escaped-bracket claim must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"));
    }

    #[test]
    fn paren_path_href_survives_whole() {
        let out = strip_md_link_claims("[wiki](https://en.wikipedia.org/wiki/Foo_(bar))");
        assert!(out.contains("https://en.wikipedia.org/wiki/Foo_(bar)"), "balanced-paren href kept intact: {out}");
    }

    #[test]
    fn reference_link_label_dropped_definition_scanned() {
        let out = strip_md_link_claims("[https://trusted.com][1]\n[1]: https://evil.io");
        assert!(!out.contains("trusted.com"), "reflink claim must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"), "definition URL stays scannable: {out}");
    }

    #[test]
    fn multibyte_label_no_panic() {
        let out = strip_md_link_claims("[🔒 sécurisé — café](https://evil.io) 日本語");
        assert!(out.contains("https://evil.io"));
        assert!(out.contains("日本語"));
    }

    // ========================================================================
    // Valid public URLs — should pass
    // ========================================================================

    #[test]
    fn valid_public_https_url_passes() {
        assert!(validate_url_not_private("https://example.com/path").is_ok(),
            "https://example.com should be allowed");
    }

    #[test]
    fn valid_public_http_url_passes() {
        assert!(validate_url_not_private("http://example.com").is_ok(),
            "http://example.com should be allowed");
    }

    #[test]
    fn valid_public_ip_8888_passes() {
        assert!(validate_url_not_private("https://8.8.8.8/dns").is_ok(),
            "8.8.8.8 (Google DNS) is a public IP and should be allowed");
    }

    #[test]
    fn valid_public_ip_1111_passes() {
        assert!(validate_url_not_private("https://1.1.1.1").is_ok(),
            "1.1.1.1 (Cloudflare DNS) is a public IP and should be allowed");
    }

    #[test]
    fn valid_url_with_port_passes() {
        assert!(validate_url_not_private("https://example.com:8080/api").is_ok(),
            "URL with port on public domain should be allowed");
    }

    // ========================================================================
    // Loopback addresses — should be rejected
    // ========================================================================

    #[test]
    fn localhost_rejected() {
        let result = validate_url_not_private("http://localhost/secret");
        assert!(result.is_err(), "localhost should be rejected");
    }

    #[test]
    fn ip_127_0_0_1_rejected() {
        let result = validate_url_not_private("http://127.0.0.1/admin");
        assert!(result.is_err(), "127.0.0.1 (loopback) should be rejected");
    }

    #[test]
    fn ip_127_255_255_255_rejected() {
        let result = validate_url_not_private("http://127.255.255.255");
        assert!(result.is_err(), "127.255.255.255 (loopback range) should be rejected");
    }

    // ========================================================================
    // Private IP ranges — should be rejected
    // ========================================================================

    #[test]
    fn private_class_a_10_rejected() {
        let result = validate_url_not_private("http://10.0.0.1/internal");
        assert!(result.is_err(), "10.0.0.1 (private class A) should be rejected");
    }

    #[test]
    fn private_class_b_172_16_rejected() {
        let result = validate_url_not_private("http://172.16.0.1/internal");
        assert!(result.is_err(), "172.16.0.1 (private class B) should be rejected");
    }

    #[test]
    fn private_class_b_172_31_rejected() {
        let result = validate_url_not_private("http://172.31.255.255");
        assert!(result.is_err(), "172.31.255.255 (private class B upper bound) should be rejected");
    }

    #[test]
    fn private_class_c_192_168_rejected() {
        let result = validate_url_not_private("http://192.168.1.1/router");
        assert!(result.is_err(), "192.168.1.1 (private class C) should be rejected");
    }

    // ========================================================================
    // Special addresses — should be rejected
    // ========================================================================

    #[test]
    fn link_local_169_254_rejected() {
        let result = validate_url_not_private("http://169.254.1.1");
        assert!(result.is_err(), "169.254.1.1 (link-local) should be rejected");
    }

    #[test]
    fn cgn_100_64_rejected() {
        let result = validate_url_not_private("http://100.64.0.1");
        assert!(result.is_err(), "100.64.0.1 (CGN / shared address space) should be rejected");
    }

    #[test]
    fn cgn_100_127_rejected() {
        let result = validate_url_not_private("http://100.127.255.255");
        assert!(result.is_err(), "100.127.255.255 (CGN upper bound) should be rejected");
    }

    #[test]
    fn broadcast_255_rejected() {
        let result = validate_url_not_private("http://255.255.255.255");
        assert!(result.is_err(), "255.255.255.255 (broadcast) should be rejected");
    }

    #[test]
    fn unspecified_0_0_0_0_rejected() {
        let result = validate_url_not_private("http://0.0.0.0");
        assert!(result.is_err(), "0.0.0.0 (unspecified) should be rejected");
    }

    // ========================================================================
    // IPv6 addresses — should be rejected
    // ========================================================================

    #[test]
    fn ipv6_loopback_rejected() {
        let result = validate_url_not_private("http://[::1]/secret");
        assert!(result.is_err(), "::1 (IPv6 loopback) should be rejected");
    }

    #[test]
    fn ipv6_unique_local_fc00_rejected() {
        let result = validate_url_not_private("http://[fc00::1]");
        assert!(result.is_err(), "fc00::1 (IPv6 unique-local) should be rejected");
    }

    #[test]
    fn ipv6_unique_local_fd00_rejected() {
        let result = validate_url_not_private("http://[fd00::1]");
        assert!(result.is_err(), "fd00::1 (IPv6 unique-local) should be rejected");
    }

    #[test]
    fn ipv6_link_local_fe80_rejected() {
        let result = validate_url_not_private("http://[fe80::1]");
        assert!(result.is_err(), "fe80::1 (IPv6 link-local) should be rejected");
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_rejected() {
        let result = validate_url_not_private("http://[::ffff:127.0.0.1]");
        assert!(result.is_err(), "::ffff:127.0.0.1 (IPv4-mapped loopback) should be rejected");
    }

    #[test]
    fn ipv4_mapped_ipv6_private_rejected() {
        let result = validate_url_not_private("http://[::ffff:192.168.1.1]");
        assert!(result.is_err(), "::ffff:192.168.1.1 (IPv4-mapped private) should be rejected");
    }

    // ========================================================================
    // Domain name restrictions
    // ========================================================================

    #[test]
    fn dot_local_domain_rejected() {
        let result = validate_url_not_private("http://mydevice.local/api");
        assert!(result.is_err(), ".local domain should be rejected");
    }

    #[test]
    fn dot_internal_domain_rejected() {
        let result = validate_url_not_private("http://service.internal/health");
        assert!(result.is_err(), ".internal domain should be rejected");
    }

    // ========================================================================
    // Scheme restrictions
    // ========================================================================

    #[test]
    fn ftp_scheme_rejected() {
        let result = validate_url_not_private("ftp://example.com/file.txt");
        assert!(result.is_err(), "ftp:// scheme should be rejected");
        assert_eq!(result.unwrap_err(), "Only HTTP(S) URLs are allowed");
    }

    #[test]
    fn file_scheme_rejected() {
        let result = validate_url_not_private("file:///etc/passwd");
        assert!(result.is_err(), "file:// scheme should be rejected");
        assert_eq!(result.unwrap_err(), "Only HTTP(S) URLs are allowed");
    }

    #[test]
    fn javascript_scheme_rejected() {
        let result = validate_url_not_private("javascript:alert(1)");
        assert!(result.is_err(), "javascript: scheme should be rejected");
    }

    #[test]
    fn data_scheme_rejected() {
        let result = validate_url_not_private("data:text/html,<h1>hi</h1>");
        assert!(result.is_err(), "data: scheme should be rejected");
    }

    // ========================================================================
    // Missing / invalid URL
    // ========================================================================

    #[test]
    fn no_host_rejected() {
        // http:// with no host is actually an invalid URL for the url crate
        let result = validate_url_not_private("http://");
        assert!(result.is_err(), "URL with no host should be rejected");
    }

    #[test]
    fn invalid_url_rejected() {
        let result = validate_url_not_private("not a url at all");
        assert!(result.is_err(), "invalid URL string should be rejected");
        assert_eq!(result.unwrap_err(), "Invalid URL");
    }

    #[test]
    fn empty_string_rejected() {
        let result = validate_url_not_private("");
        assert!(result.is_err(), "empty string should be rejected");
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn cgn_100_63_not_rejected() {
        // 100.63.x.x is NOT in the CGN range (100.64-100.127)
        assert!(validate_url_not_private("http://100.63.255.255").is_ok(),
            "100.63.255.255 is outside CGN range and should be allowed");
    }

    #[test]
    fn cgn_100_128_not_rejected() {
        // 100.128.x.x is NOT in the CGN range
        assert!(validate_url_not_private("http://100.128.0.1").is_ok(),
            "100.128.0.1 is outside CGN range and should be allowed");
    }

    #[test]
    fn private_172_15_not_rejected() {
        // 172.15.x.x is NOT private (private is 172.16-172.31)
        assert!(validate_url_not_private("http://172.15.255.255").is_ok(),
            "172.15.255.255 is outside private class B range and should be allowed");
    }

    #[test]
    fn private_172_32_not_rejected() {
        // 172.32.x.x is NOT private
        assert!(validate_url_not_private("http://172.32.0.1").is_ok(),
            "172.32.0.1 is outside private class B range and should be allowed");
    }
}

// ============================================================================
// Egress: where a request actually goes
// ============================================================================

/// Where a request for `url` really goes, and what it carries: the proxy's
/// address with a signed authorization when the privacy setting routes it
/// through Magnitude, or the URL itself.
pub struct Egress {
    pub url: String,
    pub auth: Option<reqwest::header::HeaderValue>,
}

impl Egress {
    /// Whether the request leaves through a proxy rather than to the host.
    pub fn proxied(&self) -> bool {
        self.auth.is_some() || crate::proxy::proxy_server_of(&self.url).is_some()
    }
}

/// Resolve the destination for `url`. Every outbound fetch of somebody else's
/// content asks here first, so the setting has exactly one place to act.
pub async fn egress(url: &str) -> Egress {
    match crate::proxy::proxied(url).await {
        Some(via) => {
            let auth = match crate::proxy::proxy_server_of(&via) {
                Some(server) => crate::proxy::proxy_authorization(&server).await,
                None => None,
            };
            Egress { url: via, auth }
        }
        None => Egress { url: url.to_string(), auth: None },
    }
}

/// A request for `url` on `client`, already pointed at the right place and
/// carrying the proxy authorization when there is one.
pub async fn proxied_request(client: &HttpClient, method: reqwest::Method, url: &str) -> Req {
    let e = egress(url).await;
    match e.auth {
        Some(v) => client.with_lane(Lane::Account).request(method, &e.url).header(reqwest::header::AUTHORIZATION, v),
        None => client.request(method, &e.url),
    }
}

/// The HTTP status the SOURCE gives for `url`: 2xx means it serves, 404/410
/// means it is gone. `None` means nothing definitive could be learned (the
/// host, or the proxy, was unreachable). Through the proxy a HEAD carries no
/// body, so one byte is asked for instead and the source's status read from
/// the proxy's answer; direct, a HEAD is tried first and a one-byte GET when
/// the host refuses HEAD.
pub async fn remote_status(url: &str, timeout: std::time::Duration) -> Option<u16> {
    let e = egress(url).await;
    let client = build_http_client(timeout).ok()?;
    let client = if e.auth.is_some() { client.with_lane(Lane::Account) } else { client };
    let with = |req: Req| match &e.auth {
        Some(v) => req.header(reqwest::header::AUTHORIZATION, v.clone()),
        None => req,
    };
    if e.proxied() {
        let resp = with(client.get(&e.url)).header(reqwest::header::RANGE, "bytes=0-0").send().await.ok()?;
        if resp.status().is_success() {
            return Some(200);
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        return body.get("source_status").and_then(|v| v.as_u64()).map(|v| v as u16);
    }
    let head = with(client.head(&e.url)).send().await.ok()?;
    let status = head.status();
    if status == reqwest::StatusCode::METHOD_NOT_ALLOWED || status == reqwest::StatusCode::NOT_IMPLEMENTED {
        let r = with(client.get(&e.url)).header(reqwest::header::RANGE, "bytes=0-0").send().await.ok()?;
        return Some(r.status().as_u16());
    }
    Some(status.as_u16())
}

// ============================================================================
// Remote File Size
// ============================================================================

/// Get the size of a remote file via HEAD request or Range fallback.
/// Returns None if the URL is private, unreachable, or size can't be determined.
pub async fn get_remote_file_size(url: &str) -> Option<u64> {
    validate_url_not_private(url).ok()?;
    let client = build_http_client(std::time::Duration::from_secs(8)).ok()?;

    // Method 1: HEAD request
    if let Ok(head_res) = proxied_request(&client, reqwest::Method::HEAD, url).await.send().await {
        if let Some(length) = head_res.content_length() {
            if length > 0 {
                return Some(length);
            }
        }
    }

    // Method 2: Range request fallback
    if let Ok(partial_res) = proxied_request(&client, reqwest::Method::GET, url)
        .await
        .header("Range", "bytes=0-1")
        .send()
        .await
    {
        if let Some(content_range) = partial_res.headers().get("content-range") {
            if let Ok(range_str) = content_range.to_str() {
                if let Some(size_part) = range_str.split('/').nth(1) {
                    if let Ok(size) = size_part.parse::<u64>() {
                        return Some(size);
                    }
                }
            }
        }
        if let Some(length) = partial_res.content_length() {
            if length > 100 {
                return Some(length);
            }
        }
    }

    None
}

/// Lift the open-file ceiling to what the OS allows.
///
/// A relay pool authenticates every plane on its own socket and the SQLite pool
/// holds dozens of handles, so a busy session idles near 200 descriptors. macOS
/// launches GUI apps with a soft limit of 256; past it every socket, file and
/// child process fails, and the first system UI to lazily load a resource
/// bundle traps the process. Returns the new soft limit, `None` if unchanged.
#[allow(clippy::unnecessary_cast)] // rlim_t is u32 on 32-bit Android
pub fn raise_fd_limit() -> Option<u64> {
    #[cfg(not(unix))]
    {
        None
    }
    #[cfg(unix)]
    {
        // macOS refuses a soft limit above OPEN_MAX (sys/syslimits.h) even under
        // an unlimited hard limit; Linux refuses one above nr_open.
        #[cfg(target_os = "macos")]
        const CEILING: libc::rlim_t = 10240;
        #[cfg(not(target_os = "macos"))]
        const CEILING: libc::rlim_t = 1 << 20;

        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: plain libc calls on a stack struct we own.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
            return None;
        }
        let target = lim.rlim_max.min(CEILING);
        if target <= lim.rlim_cur {
            return None;
        }
        lim.rlim_cur = target;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
            return None;
        }
        Some(target as u64)
    }
}

#[cfg(all(test, unix))]
mod fd_limit_tests {
    #[test]
    fn soft_limit_reaches_the_ceiling_and_is_never_lowered() {
        let mut before = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut before) }, 0);

        super::raise_fd_limit();

        let mut after = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut after) }, 0);
        #[cfg(target_os = "macos")]
        let ceiling: libc::rlim_t = 10240;
        #[cfg(not(target_os = "macos"))]
        let ceiling: libc::rlim_t = 1 << 20;
        assert!(after.rlim_cur >= before.rlim_cur);
        assert!(after.rlim_cur >= after.rlim_max.min(ceiling));
    }
}

#[cfg(test)]
mod user_agent_tests {
    use super::*;

    /// Magnitude refuses a preview request whose agent does not start with
    /// `Vector/`; this is the string every request will carry.
    #[test]
    fn the_client_names_itself_as_vector() {
        assert!(user_agent().starts_with("Vector/"), "{}", user_agent());
        set_app_version("9.9.9-test");
        // Either the app's version or, if another test set it first, that
        // one: in both cases still Vector's.
        assert!(user_agent().starts_with("Vector/"), "{}", user_agent());
    }
}
