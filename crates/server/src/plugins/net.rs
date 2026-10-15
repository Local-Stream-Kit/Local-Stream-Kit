//! Server code's web requests (`fetch`) and WebSockets (`connect`): only to
//! the hosts in the plugin's manifest (redirects too), never to the app
//! itself, and no answer or message bigger than [`MAX_BODY`].
//!
//! The app listens on 127.0.0.1, and its panel API trusts a connection
//! without an `Origin`, as server code's are. So a connection to this PC
//! (a loopback or unspecified address) on the app's own port is refused,
//! whatever the host is called: the host name is resolved here, the
//! addresses checked, and the connection made to an address that was
//! checked, so a second DNS answer can't differ. Other ports on this PC
//! (obs-websocket's) stay open to the plugins allowed `localhost`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use protocol::plugin::host_allowed;
use reqwest::header::{self, HeaderName};
use reqwest::{Method, StatusCode, Url};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// The biggest answer, and socket message, either way.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// Redirects followed, at most; the next one is handed back as it is.
const MAX_REDIRECTS: usize = 5;

/// The ports the app answers on: the one it listens on, and the config's
/// (which takes over after a restart).
type AppPorts = Arc<dyn Fn() -> [u16; 2] + Send + Sync>;

/// One plugin's way out.
#[derive(Clone)]
pub struct Net {
    hosts: Arc<Vec<String>>,
    /// For the app's ports: its resolver leaves this PC's addresses out,
    /// and it uses no proxy (which would resolve the name itself).
    guarded: reqwest::Client,
    /// For any other port.
    open: reqwest::Client,
    app_ports: AppPorts,
}

/// A web request's answer.
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

impl Net {
    pub fn new(
        hosts: Vec<String>,
        app_ports: impl Fn() -> [u16; 2] + Send + Sync + 'static,
        timeout: std::time::Duration,
    ) -> Result<Net, String> {
        let client = || {
            reqwest::Client::builder()
                // Followed here, each checked: see `fetch`.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(timeout)
                .user_agent(concat!("LocalStreamKit/", env!("CARGO_PKG_VERSION")))
        };
        let guarded =
            client().no_proxy().dns_resolver(NotThisPc).build().map_err(|error| format!("{error:#}"))?;
        let open = client().build().map_err(|error| format!("{error:#}"))?;
        Ok(Net { hosts: Arc::new(hosts), guarded, open, app_ports: Arc::new(app_ports) })
    }

    fn is_app_port(&self, port: u16) -> bool {
        (self.app_ports)().contains(&port)
    }

    /// Checks a URL a plugin asked for: one of the given schemes, one of
    /// its hosts, and not the app itself.
    pub fn check(&self, url: &str, schemes: [&str; 2]) -> Result<Url, String> {
        let url = Url::parse(url).map_err(|error| format!("not a URL: {error}"))?;
        if !schemes.contains(&url.scheme()) {
            return Err(format!("only {} URLs", schemes.join(" and ")));
        }
        self.allowed_host(&url)?;
        self.not_the_app(&url)?;
        Ok(url)
    }

    fn allowed_host(&self, url: &Url) -> Result<(), String> {
        let host = url.host_str().unwrap_or_default();
        if !host_allowed(&self.hosts, host) {
            return Err(format!("{host} isn't one of the hosts in the plugin's plugin.json"));
        }
        Ok(())
    }

    /// An address written in the URL (`127.0.0.1`, `[::1]`, `0`): this PC
    /// on the app's port is refused. (A name is checked when it's
    /// resolved.)
    fn not_the_app(&self, url: &Url) -> Result<(), String> {
        let port = url.port_or_known_default().unwrap_or_default();
        if literal_ip(url).is_some_and(is_this_pc) && self.is_app_port(port) {
            return Err(the_app(url.host_str().unwrap_or_default()));
        }
        Ok(())
    }

    /// Sends a request, following up to [`MAX_REDIRECTS`] redirects to
    /// hosts it may reach (one to another host is handed back as it is),
    /// and reads the answer, up to [`MAX_BODY`].
    pub async fn fetch(
        &self,
        mut method: Method,
        mut url: Url,
        mut headers: Vec<(String, String)>,
        mut body: Vec<u8>,
    ) -> Result<Answer, String> {
        let mut redirects = 0;
        loop {
            let port = url.port_or_known_default().unwrap_or_default();
            let client = if self.is_app_port(port) { &self.guarded } else { &self.open };
            let mut request = client.request(method.clone(), url.clone());
            for (name, value) in &headers {
                request = request.header(name, value);
            }
            if !body.is_empty() {
                request = request.body(body.clone());
            }
            let response = request.send().await.map_err(|error| describe(&error))?;
            if redirects < MAX_REDIRECTS
                && let Some(next) = redirect(&url, &response)
                && self.allowed_host(&next).is_ok()
            {
                self.not_the_app(&next)?;
                redirects += 1;
                let status = response.status();
                if (status == StatusCode::SEE_OTHER && method != Method::HEAD)
                    || (matches!(status, StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND)
                        && method == Method::POST)
                {
                    method = Method::GET;
                    body.clear();
                    headers.retain(|(name, _)| !is_about_the_body(name));
                }
                if next.host_str() != url.host_str()
                    || next.port_or_known_default() != url.port_or_known_default()
                    || next.scheme() != url.scheme()
                {
                    headers.retain(|(name, _)| !is_sensitive(name));
                }
                url = next;
                continue;
            }
            return read(response).await;
        }
    }

    /// Opens a WebSocket to `url` (checked with [`Net::check`]).
    pub async fn connect(&self, url: &Url, config: WebSocketConfig) -> Result<Socket, String> {
        let port = url.port_or_known_default().ok_or("no port")?;
        let addresses = match (literal_ip(url), url.host_str()) {
            (Some(ip), _) => vec![SocketAddr::new(ip, port)],
            (None, Some(name)) if self.is_app_port(port) => not_this_pc(name, port).await?,
            (None, Some(name)) => resolve(name, port).await?,
            (None, None) => return Err("no host".into()),
        };
        let stream = TcpStream::connect(&addresses[..]).await.map_err(|error| error.to_string())?;
        let (socket, _) =
            tokio_tungstenite::client_async_tls_with_config(url.as_str(), stream, Some(config), None)
                .await
                .map_err(|error| error.to_string())?;
        Ok(socket)
    }
}

/// Where a redirect answer sends it, if it's one to follow.
fn redirect(from: &Url, response: &reqwest::Response) -> Option<Url> {
    let status = response.status();
    if !matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    ) {
        return None;
    }
    let location = response.headers().get(header::LOCATION)?.to_str().ok()?;
    let next = from.join(location).ok()?;
    matches!(next.scheme(), "http" | "https").then_some(next)
}

/// Reads an answer, refusing one bigger than [`MAX_BODY`] as soon as it
/// is (one that doesn't say its length too).
async fn read(mut response: reqwest::Response) -> Result<Answer, String> {
    let too_big = || format!("the answer is bigger than {} MB", MAX_BODY / 1024 / 1024);
    if response.content_length().is_some_and(|length| length > MAX_BODY as u64) {
        return Err(too_big());
    }
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| describe(&error))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Answer { status, headers, body })
}

fn is_about_the_body(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("content-") || name == "transfer-encoding"
}

/// Headers not carried to another host on a redirect (as reqwest does).
fn is_sensitive(name: &str) -> bool {
    HeaderName::from_bytes(name.as_bytes()).is_ok_and(|name| {
        [header::AUTHORIZATION, header::COOKIE, header::PROXY_AUTHORIZATION, header::WWW_AUTHENTICATE]
            .contains(&name)
            || name == "cookie2"
    })
}

/// An error with what caused it (reqwest's own says only "error sending
/// request").
fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let said = cause.to_string();
        if !text.contains(&said) {
            text = format!("{text}: {said}");
        }
        source = cause.source();
    }
    text
}

/// The IP address written as a URL's host, if it is one.
fn literal_ip(url: &Url) -> Option<IpAddr> {
    url.host_str()?.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

/// This PC: a loopback address (`127.x.x.x`, `::1`, as IPv4 in IPv6 too),
/// or the unspecified one (`0.0.0.0`, `::`), which reaches it too.
fn is_this_pc(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    ip.is_loopback() || ip.is_unspecified()
}

fn the_app(host: &str) -> String {
    format!("server code can't reach the app itself ({host} is this PC, on the app's port)")
}

async fn resolve(name: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addresses: Vec<SocketAddr> =
        tokio::net::lookup_host((name, port)).await.map_err(|error| format!("{name}: {error}"))?.collect();
    if addresses.is_empty() {
        return Err(format!("{name} has no address"));
    }
    Ok(addresses)
}

/// `name`'s addresses, leaving out this PC's; none left is an error.
async fn not_this_pc(name: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addresses: Vec<SocketAddr> =
        resolve(name, port).await?.into_iter().filter(|address| !is_this_pc(address.ip())).collect();
    if addresses.is_empty() {
        return Err(the_app(name));
    }
    Ok(addresses)
}

/// The resolver for requests to the app's ports.
struct NotThisPc;

impl reqwest::dns::Resolve for NotThisPc {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().to_string();
        Box::pin(async move {
            let addresses = not_this_pc(&name, 0).await?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A web server on a free local port answering every request with
    /// what `answer` makes of its path; its port.
    async fn server(answer: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let answer = Arc::new(answer);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let answer = Arc::clone(&answer);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let request = String::from_utf8_lossy(&request).into_owned();
                    let path = request.split(' ').nth(1).unwrap_or_default().to_string();
                    let _ = stream.write_all(&answer(&path)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        port
    }

    fn ok() -> Vec<u8> {
        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi".to_vec()
    }

    fn redirect_to(location: String) -> Vec<u8> {
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
        .into_bytes()
    }

    fn net(app: u16) -> Net {
        Net::new(vec!["localhost".into(), "127.0.0.1".into()], move || [app, 0], Duration::from_secs(5))
            .unwrap()
    }

    async fn get(net: &Net, url: &str) -> Result<Answer, String> {
        let url = net.check(url, ["http", "https"])?;
        net.fetch(Method::GET, url, Vec::new(), Vec::new()).await
    }

    #[tokio::test]
    async fn the_app_itself_is_refused_other_ports_arent() {
        let app = server(|_| ok()).await;
        let other = server(|_| ok()).await;
        let net = net(app);

        // Written as an address: refused before anything is sent.
        for url in [
            format!("http://127.0.0.1:{app}/"),
            format!("http://127.1:{app}/"),
            format!("http://[::1]:{app}/"),
            format!("http://0.0.0.0:{app}/"),
            format!("http://[::ffff:127.0.0.1]:{app}/"),
        ] {
            let hosts = vec!["127.0.0.1".into(), "[::1]".into(), "0.0.0.0".into(), "[::ffff:7f00:1]".into()];
            let net = Net::new(hosts, move || [app, 0], Duration::from_secs(5)).unwrap();
            let refused = net.check(&url, ["http", "https"]).err().unwrap_or_default();
            assert!(refused.contains("can't reach the app itself"), "{url}: {refused}");
        }
        // A name that is this PC: refused when it's resolved.
        let refused = get(&net, &format!("http://localhost:{app}/")).await.err().unwrap();
        assert!(refused.contains("can't reach the app itself"), "{refused}");
        // Another port on this PC is fine.
        assert_eq!(get(&net, &format!("http://127.0.0.1:{other}/")).await.unwrap().body, b"hi");
        assert_eq!(get(&net, &format!("http://localhost:{other}/")).await.unwrap().status, 200);
    }

    #[tokio::test]
    async fn redirects_are_checked_too() {
        let app = server(|_| ok()).await;
        let elsewhere = server(move |path| match path {
            "/to-app" => redirect_to(format!("http://127.0.0.1:{app}/")),
            "/to-app-by-name" => redirect_to(format!("http://localhost:{app}/")),
            "/away" => redirect_to("http://example.com/".into()),
            "/here" => redirect_to("/fine".into()),
            _ => ok(),
        })
        .await;
        let net = net(app);
        let url = |path: &str| format!("http://127.0.0.1:{elsewhere}{path}");
        for path in ["/to-app", "/to-app-by-name"] {
            let refused = get(&net, &url(path)).await.err().unwrap();
            assert!(refused.contains("can't reach the app itself"), "{path}: {refused}");
        }
        // To a host it may not reach: the redirect is the answer.
        assert_eq!(get(&net, &url("/away")).await.unwrap().status, 302);
        assert_eq!(get(&net, &url("/here")).await.unwrap().body, b"hi");
    }

    #[tokio::test]
    async fn an_answer_too_big_is_refused_while_it_comes() {
        // No length: chunked, past the limit.
        let big = server(|_| {
            let chunk = vec![b'x'; 64 * 1024];
            let mut answer =
                b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
            for _ in 0..(MAX_BODY / chunk.len() + 1) {
                answer.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                answer.extend_from_slice(&chunk);
                answer.extend_from_slice(b"\r\n");
            }
            answer.extend_from_slice(b"0\r\n\r\n");
            answer
        })
        .await;
        let refused = get(&net(0), &format!("http://127.0.0.1:{big}/")).await.err().unwrap();
        assert!(refused.contains("bigger than 4 MB"), "{refused}");
    }

    #[tokio::test]
    async fn sockets_to_the_app_are_refused() {
        let app = server(|_| ok()).await;
        let net = net(app);
        let refused = net.check(&format!("ws://127.0.0.1:{app}/ws"), ["ws", "wss"]).err().unwrap();
        assert!(refused.contains("can't reach the app itself"), "{refused}");
        let url = net.check(&format!("ws://localhost:{app}/ws"), ["ws", "wss"]).unwrap();
        let refused = net.connect(&url, WebSocketConfig::default()).await.err().unwrap();
        assert!(refused.contains("can't reach the app itself"), "{refused}");
    }
}
