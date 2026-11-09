//! The email channel (spec C21 §4.3): a small SMTP submission client.
//!
//! The channel submits one message per delivery on the submission port of RFC 6409
//! with STARTTLS (RFC 3207), or on port 465 with implicit TLS (RFC 8314), and
//! authenticates with SASL PLAIN (RFC 4954) when the channel has a user name. A
//! server that does not offer STARTTLS is refused, so a message and a password never
//! cross the network in the clear. The client speaks only what a submission needs:
//! EHLO, STARTTLS, AUTH PLAIN, MAIL, RCPT, DATA and QUIT (RFC 5321). A `4xx` reply ends
//! the attempt as a transient failure that is retried, and a `5xx` reply ends the
//! delivery.
//!
//! TLS is rustls with the platform's roots, the verifier reqwest already uses, and the
//! extra roots of the server's outbound policy. The SMTP host passes the outbound
//! policy's host check, and the client connects only to the addresses it checked.

use super::Attempt;
use super::config::{Email, EmailTls};
use base64::Engine as _;
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The longest reply line read, which RFC 5321 §4.5.3.1.5 sets at 512 octets; some
/// servers send longer ones.
const MAX_LINE: usize = 4096;
/// The most lines of one reply.
const MAX_LINES: usize = 200;

/// The connection, before or after TLS.
enum Conn {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>>),
}

impl Conn {
    fn tcp(&self) -> &TcpStream {
        match self {
            Conn::Plain(r) => r.get_ref(),
            Conn::Tls(r) => &r.get_ref().sock,
        }
    }

    fn reader(&mut self) -> &mut dyn BufRead {
        match self {
            Conn::Plain(r) => r,
            Conn::Tls(r) => r.as_mut(),
        }
    }

    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        match self {
            Conn::Plain(r) => {
                let s = r.get_mut();
                s.write_all(b)?;
                s.flush()
            }
            Conn::Tls(r) => {
                let s = r.get_mut();
                s.write_all(b)?;
                s.flush()
            }
        }
    }
}

/// How one step of the transaction ended badly.
enum Stop {
    /// a connection error or a timeout, which §6.2 retries
    Io(String),
    /// a reply that was not the one expected
    Reply(u16, String),
    /// a failure that another attempt cannot fix
    Fatal(String),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Stop {
        match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                Stop::Io("the mail server did not answer in time".into())
            }
            _ => match e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
                Some(t) => Stop::Fatal(format!("TLS: {t}")),
                None => Stop::Io(format!("connection to the mail server: {e}")),
            },
        }
    }
}

struct Session {
    conn: Conn,
    deadline: Instant,
}

impl Session {
    /// Arm the socket's timeouts with the time left before the deadline.
    fn arm(&self) -> Result<(), Stop> {
        let left = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| Stop::Io("the mail server did not answer in time".into()))?;
        let t = self.conn.tcp();
        t.set_read_timeout(Some(left))?;
        t.set_write_timeout(Some(left))?;
        Ok(())
    }

    /// Read one reply: its code and the text of each line.
    fn reply(&mut self) -> Result<(u16, Vec<String>), Stop> {
        self.arm()?;
        let mut text = Vec::new();
        for _ in 0..MAX_LINES {
            let mut line = Vec::new();
            let r = self.conn.reader();
            let n = r.take(MAX_LINE as u64).read_until(b'\n', &mut line)?;
            if n == 0 {
                return Err(Stop::Io("the mail server closed the connection".into()));
            }
            let l = String::from_utf8_lossy(&line);
            let l = l.trim_end_matches(['\r', '\n']);
            let code = l
                .get(..3)
                .and_then(|c| c.parse::<u16>().ok())
                .filter(|c| (200..600).contains(c))
                .ok_or_else(|| Stop::Fatal("the mail server's reply is not SMTP".into()))?;
            let more = l.as_bytes().get(3) == Some(&b'-');
            text.push(l.get(4..).unwrap_or("").to_string());
            if !more {
                return Ok((code, text));
            }
        }
        Err(Stop::Fatal(
            "the mail server's reply has too many lines".into(),
        ))
    }

    /// Send `line` (without CRLF) and read the reply, which must have code `want`, or a
    /// code in `want..want + 100` when `want` is a multiple of 100.
    fn command(&mut self, line: &str, want: u16) -> Result<Vec<String>, Stop> {
        self.arm()?;
        self.conn.write_all(format!("{line}\r\n").as_bytes())?;
        self.expect(want)
    }

    fn expect(&mut self, want: u16) -> Result<Vec<String>, Stop> {
        let (code, text) = self.reply()?;
        let ok = if want % 100 == 0 {
            code / 100 == want / 100
        } else {
            code == want
        };
        if ok {
            Ok(text)
        } else {
            Err(Stop::Reply(code, text.join(" ")))
        }
    }

    /// EHLO: the extensions the server lists, each as its upper-cased words. The
    /// first line is the greeting.
    fn ehlo(&mut self, name: &str) -> Result<Vec<Vec<String>>, Stop> {
        let lines = self.command(&format!("EHLO {name}"), 250)?;
        Ok(lines
            .iter()
            .skip(1)
            .map(|l| l.split_whitespace().map(str::to_ascii_uppercase).collect())
            .collect())
    }

    fn start_tls(self, host: &str, config: Arc<rustls::ClientConfig>) -> Result<Session, Stop> {
        let Conn::Plain(r) = self.conn else {
            return Err(Stop::Fatal("TLS is already on".into()));
        };
        if !r.buffer().is_empty() {
            // a server that sends data past its STARTTLS reply could inject commands
            return Err(Stop::Fatal(
                "the mail server sent data before the TLS handshake".into(),
            ));
        }
        let tcp = r.into_inner();
        Ok(Session {
            conn: tls(tcp, host, config)?,
            deadline: self.deadline,
        })
    }
}

fn tls(tcp: TcpStream, host: &str, config: Arc<rustls::ClientConfig>) -> Result<Conn, Stop> {
    let name = match host.parse::<IpAddr>() {
        Ok(ip) => ServerName::IpAddress(ip.into()),
        Err(_) => ServerName::try_from(host.to_string())
            .map_err(|_| Stop::Fatal(format!("{host:?} is not a host name for TLS")))?,
    };
    let c = rustls::ClientConnection::new(config, name)
        .map_err(|e| Stop::Fatal(format!("TLS: {e}")))?;
    let mut s = rustls::StreamOwned::new(c, tcp);
    // the handshake runs now, so that its failure is reported as such
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock)?;
    }
    Ok(Conn::Tls(Box::new(BufReader::new(s))))
}

/// The client's TLS configuration: the platform's roots and the outbound policy's
/// extra roots.
fn tls_config(
    policy: &sparkles::outbound::OutboundPolicy,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut extra: Vec<CertificateDer<'static>> = Vec::new();
    if let Some(pem) = &policy.tls.extra_roots_pem {
        use rustls::pki_types::pem::PemObject;
        for c in CertificateDer::pem_slice_iter(pem) {
            extra.push(c.map_err(|e| format!("the outbound policy's extra roots: {e}"))?);
        }
    }
    let verifier =
        rustls_platform_verifier::Verifier::new_with_extra_roots(extra, provider.clone())
            .map_err(|e| format!("TLS: {e}"))?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The name the client gives in EHLO: the host name when it is a domain, else an
/// address literal.
fn ehlo_name() -> String {
    let h = super::host_name();
    let ok = !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if ok { h } else { "[127.0.0.1]".into() }
}

/// Connect to the first of `addrs` that answers, within `connect` each.
fn connect(addrs: &[IpAddr], port: u16, connect: Duration) -> Result<TcpStream, Stop> {
    let mut last = None;
    for ip in addrs {
        match TcpStream::connect_timeout(&SocketAddr::new(*ip, port), connect) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => Stop::from(e),
        None => Stop::Io("the mail server has no address".into()),
    })
}

/// One attempt to submit the message of `env` through channel `name`. `password` is
/// the value of the channel's password secret.
pub(super) fn send(
    policy: &sparkles::outbound::OutboundPolicy,
    name: &str,
    e: &Email,
    password: Option<&str>,
    env: &Value,
    timeout: Duration,
) -> Attempt {
    let addrs = match policy.check_host(&e.host) {
        Ok(a) => a,
        Err(sparkles::outbound::Failure::Refused(m)) => return Attempt::Refused(m),
        Err(f) => return Attempt::Retry(f.to_string(), None),
    };
    let config = match tls_config(policy) {
        Ok(c) => c,
        Err(m) => return Attempt::Failed(format!("channel {name}: {m}")),
    };
    let deadline = Instant::now() + timeout;
    let started = (|| -> Result<(), Stop> {
        let tcp = connect(&addrs, e.port(), policy.connect_timeout.min(timeout))?;
        let conn = match e.tls {
            EmailTls::Tls => {
                tcp.set_read_timeout(Some(timeout))?;
                tcp.set_write_timeout(Some(timeout))?;
                tls(tcp, &e.host, config.clone())?
            }
            EmailTls::Starttls => Conn::Plain(BufReader::new(tcp)),
        };
        let mut s = Session { conn, deadline };
        s.expect(220)?;
        let me = ehlo_name();
        let mut ext = s.ehlo(&me)?;
        if e.tls == EmailTls::Starttls {
            if !ext
                .iter()
                .any(|x| x.first().is_some_and(|k| k == "STARTTLS"))
            {
                return Err(Stop::Fatal(
                    "the mail server does not offer STARTTLS, and the channel never sends in the clear".into(),
                ));
            }
            s.command("STARTTLS", 220)?;
            s = s.start_tls(&e.host, config.clone())?;
            ext = s.ehlo(&me)?;
        }
        if let Some(user) = &e.username {
            let pw = password.unwrap_or("");
            let plain = ext.iter().any(|x| {
                x.first().is_some_and(|k| k == "AUTH") && x.iter().skip(1).any(|m| m == "PLAIN")
            });
            if !plain {
                return Err(Stop::Fatal(
                    "the mail server does not offer AUTH PLAIN".into(),
                ));
            }
            let token = base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{pw}"));
            // the reply to AUTH never quotes the token, and the token is not logged
            s.command(&format!("AUTH PLAIN {token}"), 235)?;
        }
        s.command(&format!("MAIL FROM:<{}>", e.from), 250)?;
        for to in &e.to {
            s.command(&format!("RCPT TO:<{to}>"), 250)?;
        }
        s.command("DATA", 354)?;
        let msg = message(e, env);
        s.arm()?;
        s.conn.write_all(&msg)?;
        s.expect(250)?;
        let _ = s.command("QUIT", 221);
        Ok(())
    })();
    match started {
        Ok(()) => Attempt::Ok(250),
        Err(Stop::Io(m)) => Attempt::Retry(format!("channel {name}: {m}"), None),
        Err(Stop::Fatal(m)) => Attempt::Failed(format!("channel {name}: {m}")),
        Err(Stop::Reply(code, text)) => {
            let text: String = text.chars().take(200).collect();
            let m = format!("SMTP {code}: {text}");
            if code / 100 == 4 {
                Attempt::Retry(m, None)
            } else {
                Attempt::Failed(m)
            }
        }
    }
}

// ---------------------------------------------------------------- the message ------

/// The message of `env` (RFC 5322), dot-stuffed and ended for DATA: the title as the
/// subject, and a plain-text body of the summary, the link and the data.
pub(super) fn message(e: &Email, env: &Value) -> Vec<u8> {
    let s = |k: &str| env[k].as_str().unwrap_or("").to_string();
    let time = chrono::DateTime::parse_from_rfc3339(&s("time"))
        .map(|t| t.to_rfc2822())
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc2822());
    let server = s("server");
    let domain = e.from.rsplit_once('@').map_or("sparkles", |(_, d)| d);
    let mut body = String::new();
    body.push_str(&s("summary"));
    body.push_str("\n\n");
    if let Some(l) = env["link"].as_str() {
        body.push_str(l);
        body.push_str("\n\n");
    }
    body.push_str(&format!("Event: {}\n", s("type")));
    body.push_str(&format!("Severity: {}\n", s("severity")));
    body.push_str(&format!("Server: {server}\n"));
    if let Some(d) = env["dataset"].as_str() {
        body.push_str(&format!("Dataset: {d}\n"));
    }
    body.push_str(&format!("Time: {}\n", s("time")));
    body.push_str(&format!("ID: {}\n", s("id")));
    if env["data"].as_object().is_some_and(|o| !o.is_empty()) {
        body.push_str("\nData:\n");
        body.push_str(&serde_json::to_string_pretty(&env["data"]).unwrap_or_default());
        body.push('\n');
    }
    let qp = !body.is_ascii() || body.lines().any(|l| l.len() > 998);
    let mut h = String::new();
    h.push_str(&format!("Date: {time}\r\n"));
    h.push_str(&format!("From: {}\r\n", e.from));
    h.push_str(&format!("To: {}\r\n", e.to.join(", ")));
    h.push_str(&format!("Subject: {}\r\n", subject(&s("title"))));
    h.push_str(&format!("Message-ID: <{}@{domain}>\r\n", s("id")));
    h.push_str("MIME-Version: 1.0\r\n");
    h.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    h.push_str(if qp {
        "Content-Transfer-Encoding: quoted-printable\r\n"
    } else {
        "Content-Transfer-Encoding: 7bit\r\n"
    });
    h.push_str(&format!("X-Sparkles-Event: {}\r\n", s("type")));
    h.push_str("\r\n");
    let text = if qp {
        quoted_printable(&body)
    } else {
        body.replace('\n', "\r\n")
    };
    let mut out = Vec::with_capacity(h.len() + text.len() + 8);
    for line in format!("{h}{text}").split_inclusive("\r\n") {
        if line.starts_with('.') {
            out.push(b'.');
        }
        out.extend_from_slice(line.as_bytes());
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

/// A header value that is printable ASCII as is, and otherwise encoded words of RFC
/// 2047 in Base64, folded so that no line passes 78 characters.
pub(super) fn subject(t: &str) -> String {
    let t: String = t
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if t.is_ascii() && t.len() <= 900 {
        return t;
    }
    // at most 45 bytes of UTF-8 per word, which Base64 makes 60 characters
    let mut words = Vec::new();
    let mut cur = String::new();
    for c in t.chars() {
        if cur.len() + c.len_utf8() > 45 {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .iter()
        .map(|w| {
            format!(
                "=?utf-8?B?{}?=",
                base64::engine::general_purpose::STANDARD.encode(w)
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n ")
}

/// `text` in quoted-printable (RFC 2045 §6.7), with CRLF line ends and soft breaks
/// before 76 characters.
pub(super) fn quoted_printable(text: &str) -> String {
    let mut out = String::new();
    for line in text.split('\n') {
        let mut col = 0;
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let enc = match b {
                b'=' => true,
                b' ' | b'\t' => last,
                33..=126 => false,
                _ => true,
            };
            let w = if enc { 3 } else { 1 };
            if col + w > 75 {
                out.push_str("=\r\n");
                col = 0;
            }
            if enc {
                out.push_str(&format!("={b:02X}"));
            } else {
                out.push(b as char);
            }
            col += w;
        }
        out.push_str("\r\n");
    }
    // the split leaves one empty line after a final newline
    if text.ends_with('\n') {
        out.truncate(out.len() - 2);
    }
    out
}
