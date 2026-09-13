//! Reaching the engine: host and port, TLS, and what to say when it cannot be reached.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use brain_proto::brain_client::BrainClient;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Status};

pub const DEFAULT_PORT: u16 = 6969;
const LOCAL_CA: &str = ".brain/tls/engine.pem";

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

/// A client for `host`, trusting `ca` (or the local certificate for localhost, or the web's
/// roots). Returns the client and the host as `name:port`.
pub fn connect(host: &str, ca: Option<&Path>) -> Result<(BrainClient<Channel>, String), String> {
    let (url, name, tls) = endpoint_url(host);
    let mut endpoint = Endpoint::from_shared(url.clone())
        .map_err(|e| format!("bad host {host}: {e}"))?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(600));
    if tls {
        let mut config = ClientTlsConfig::new().domain_name(name.clone());
        let local = matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1");
        let ca = ca.map(Path::to_path_buf).or_else(|| {
            local
                .then(|| PathBuf::from(LOCAL_CA))
                .filter(|p| p.exists())
        });
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
    Ok((BrainClient::new(endpoint.connect_lazy()), host))
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
    format!("cannot reach engine at {host} (start it with make run): {detail}")
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
