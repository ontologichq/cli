//! Reaching the engine: host and port, TLS, signing in on every call, and what to say when
//! the engine cannot be reached.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use brain_proto::brain_client::BrainClient;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Request, Status};

pub const DEFAULT_PORT: u16 = 6969;
/// Certificates the CLI trusts without `--ca`: `engine.pem` (written by a local engine) for
/// localhost, `<host>.pem` (from `make server-ca`) for other hosts.
const CA_DIR: &str = ".brain/tls";

pub type Client = BrainClient<InterceptedService<Channel, SignIn>>;

/// Adds the `user` and `key` metadata to every call; the engine checks them before each one.
#[derive(Clone)]
pub struct SignIn {
    user: MetadataValue<Ascii>,
    key: MetadataValue<Ascii>,
}

impl Interceptor for SignIn {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request.metadata_mut().insert("user", self.user.clone());
        request.metadata_mut().insert("key", self.key.clone());
        Ok(request)
    }
}

fn is_local(name: &str) -> bool {
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

/// `localhost` -> (`https://localhost:6969`, `localhost`); an `http://` host stays plain.
pub fn endpoint_url(host: &str) -> (String, String, bool) {
    let (scheme, rest, tls) = match host {
        h if h.starts_with("http://") => ("http", &h["http://".len()..], false),
        h if h.starts_with("https://") => ("https", &h["https://".len()..], true),
        h => ("https", h, true),
    };
    let rest = rest.trim_end_matches('/');
    let has_port = match rest.rsplit_once(':') {
        Some((_, port)) => port.parse::<u16>().is_ok() && !rest.ends_with(']'),
        None => false,
    };
    let authority = match has_port {
        true => rest.to_string(),
        false => format!("{rest}:{DEFAULT_PORT}"),
    };
    let name = authority
        .rsplit_once(':')
        .map_or(authority.as_str(), |(name, _)| name)
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    (format!("{scheme}://{authority}"), name, tls)
}

/// A client for `host` that signs in as `user` with `key`, trusting `ca` (or a certificate
/// in `.brain/tls`, or the web's roots). Returns the client and the host as `name:port`.
pub fn connect(
    host: &str,
    ca: Option<&Path>,
    user: &str,
    key: &str,
) -> Result<(Client, String), String> {
    let sign_in = SignIn {
        user: user
            .parse()
            .map_err(|_| format!("user name {user:?} is not plain text"))?,
        key: key
            .parse()
            .map_err(|_| "the key is not plain text".to_string())?,
    };
    let (url, name, tls) = endpoint_url(host);
    let mut endpoint = Endpoint::from_shared(url.clone())
        .map_err(|e| format!("bad host {host}: {e}"))?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(600));
    if tls {
        let mut config = ClientTlsConfig::new().domain_name(name.clone());
        let known = match is_local(&name) {
            true => PathBuf::from(CA_DIR).join("engine.pem"),
            false => PathBuf::from(CA_DIR).join(format!("{name}.pem")),
        };
        let ca = ca
            .map(Path::to_path_buf)
            .or_else(|| known.exists().then_some(known));
        config = match ca {
            Some(path) => {
                let pem = fs::read_to_string(&path)
                    .map_err(|e| format!("reading {}: {e}", path.display()))?;
                config.ca_certificate(Certificate::from_pem(pem))
            }
            None => config.with_webpki_roots(),
        };
        endpoint = endpoint
            .tls_config(config)
            .map_err(|e| format!("TLS setup for {url}: {e}"))?;
    }
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string();
    let client = BrainClient::with_interceptor(endpoint.connect_lazy(), sign_in);
    Ok((client, host))
}

/// What went wrong, in words: unreachable engines and TLS failures get a hint.
pub fn describe(status: &Status, host: &str) -> String {
    if status.code() != Code::Unavailable {
        return status.message().to_string();
    }
    // The innermost cause says what actually failed: refused, timed out, bad certificate.
    let mut detail = status.message().to_string();
    let mut source = std::error::Error::source(status);
    while let Some(e) = source {
        detail = e.to_string();
        source = e.source();
    }
    let hint = match is_local(endpoint_url(host).1.as_str()) {
        true => " (start it with make run)",
        false => "",
    };
    format!("cannot reach engine at {host}{hint}: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_get_a_scheme_and_the_default_port() {
        assert_eq!(
            endpoint_url("localhost"),
            ("https://localhost:6969".into(), "localhost".into(), true)
        );
        assert_eq!(
            endpoint_url("brain.example.com:443"),
            (
                "https://brain.example.com:443".into(),
                "brain.example.com".into(),
                true
            )
        );
        assert_eq!(
            endpoint_url("http://127.0.0.1:7000/"),
            ("http://127.0.0.1:7000".into(), "127.0.0.1".into(), false)
        );
        assert_eq!(
            endpoint_url("https://brain.example.com"),
            (
                "https://brain.example.com:6969".into(),
                "brain.example.com".into(),
                true
            )
        );
    }
}
