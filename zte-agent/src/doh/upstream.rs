use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use reqwest::blocking::Client;
use reqwest::redirect::Policy;

/// Longest idle gap after which the shared HTTP client is thrown away.
///
/// Rebuilding after a quiet period avoids reusing an encrypted connection whose
/// mobile-network NAT mapping expired while the router was idle.
pub const AGENT_IDLE_MAX: Duration = Duration::from_secs(15);

/// Build an HTTPS-only upstream client. Reqwest negotiates HTTP/2 with resolvers
/// such as Quad9 while retaining HTTP/1.1 compatibility for other providers.
pub fn build_agent(timeout_ms: u32) -> Result<Client, String> {
    let timeout = Duration::from_millis(u64::from(timeout_ms));
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(timeout)
        .timeout(timeout)
        .pool_idle_timeout(Some(AGENT_IDLE_MAX))
        .build()
        .map_err(|error| format!("DoH client setup failed: {error}"))
}

/// One HTTP/2-capable client reused across upstream queries.
///
/// Rebuilt when `timeout_ms` changes or after [`AGENT_IDLE_MAX`] of inactivity.
/// Construction is lazy so a local DoT-only configuration does not depend on
/// HTTP client initialization. Cloned clients share the same connection pool.
pub struct SharedAgent {
    agent: Option<Client>,
    timeout_ms: u32,
    /// Last time the client was handed out (or rebuilt by `checkout`).
    last_used: Instant,
    #[cfg(test)]
    generation: u64,
}

impl SharedAgent {
    pub fn new(timeout_ms: u32) -> Self {
        Self::new_at(timeout_ms, Instant::now())
    }

    fn new_at(timeout_ms: u32, now: Instant) -> Self {
        Self {
            agent: None,
            timeout_ms,
            last_used: now,
            #[cfg(test)]
            generation: 0,
        }
    }

    /// Hand out the client for one request, rebuilding it first if it has been
    /// idle for longer than [`AGENT_IDLE_MAX`].
    pub fn checkout(&mut self) -> Result<Client, String> {
        self.checkout_at(Instant::now())
    }

    fn checkout_at(&mut self, now: Instant) -> Result<Client, String> {
        if self.agent.is_none() || now.saturating_duration_since(self.last_used) > AGENT_IDLE_MAX {
            self.agent = Some(build_agent(self.timeout_ms)?);
            #[cfg(test)]
            {
                self.generation += 1;
            }
        }
        self.last_used = now;
        self.agent
            .clone()
            .ok_or_else(|| "DoH client is unavailable".to_string())
    }

    /// Apply a new request timeout; the client is rebuilt only when it changed.
    pub fn set_timeout(&mut self, timeout_ms: u32) {
        if self.timeout_ms == timeout_ms {
            return;
        }
        self.timeout_ms = timeout_ms;
        self.agent = None;
    }

    /// Drop every pooled HTTP connection after the underlying WAN changes.
    /// The next query builds a fresh client and therefore cannot reuse a
    /// socket, route or mobile-network NAT mapping from the previous bearer.
    pub fn reset(&mut self) {
        self.agent = None;
        self.last_used = Instant::now();
    }

    #[cfg(test)]
    pub fn configured_timeout_ms(&self) -> u32 {
        self.timeout_ms
    }
}

pub fn query_doh(agent: &Client, dns_query: &[u8], url: &str) -> Result<Vec<u8>, String> {
    let response = agent
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/dns-message")
        .header(reqwest::header::ACCEPT, "application/dns-message")
        .body(dns_query.to_vec())
        .send()
        .map_err(|e| format!("DoH request failed: {e}"))?;
    let body = response
        .error_for_status()
        .map_err(|e| format!("DoH request failed: {e}"))?
        .bytes()
        .map_err(|e| format!("DoH read failed: {e}"))?;
    Ok(body.to_vec())
}

type TlsStream = StreamOwned<ClientConnection, TcpStream>;

pub struct DotClient {
    tls: Arc<ClientConfig>,
    stream: Option<TlsStream>,
    connected_endpoint: String,
    connected_server_name: String,
    timeout_ms: u32,
}

impl DotClient {
    pub fn new(timeout_ms: u32) -> Result<Self, String> {
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|error| format!("DoT TLS setup failed: {error}"))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(Self {
            tls: Arc::new(tls),
            stream: None,
            connected_endpoint: String::new(),
            connected_server_name: String::new(),
            timeout_ms,
        })
    }

    pub fn set_timeout(&mut self, timeout_ms: u32) {
        if self.timeout_ms != timeout_ms {
            self.timeout_ms = timeout_ms;
            self.stream = None;
        }
    }

    pub fn reset(&mut self) {
        self.stream = None;
        self.connected_endpoint.clear();
        self.connected_server_name.clear();
    }

    pub fn query(
        &mut self,
        dns_query: &[u8],
        endpoint: &str,
        server_name: &str,
    ) -> Result<Vec<u8>, String> {
        if dns_query.len() > u16::MAX as usize {
            return Err("DoT query is too large".to_string());
        }
        if self.stream.is_none()
            || self.connected_endpoint != endpoint
            || self.connected_server_name != server_name
        {
            self.connect(endpoint, server_name)?;
        }
        match self.query_connected(dns_query) {
            Ok(response) => Ok(response),
            Err(first) => {
                self.stream = None;
                self.connect(endpoint, server_name)?;
                self.query_connected(dns_query)
                    .map_err(|second| format!("{second}; retry after {first}"))
            }
        }
    }

    fn connect(&mut self, endpoint: &str, server_name: &str) -> Result<(), String> {
        let timeout = Duration::from_millis(u64::from(self.timeout_ms));
        let addresses: Vec<_> = endpoint
            .to_socket_addrs()
            .map_err(|error| format!("DoT endpoint {endpoint} could not be resolved: {error}"))?
            .collect();
        if addresses.is_empty() {
            return Err(format!("DoT endpoint {endpoint} has no address"));
        }
        let mut last_error = None;
        let mut tcp = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(stream) => {
                    tcp = Some(stream);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let tcp = tcp.ok_or_else(|| {
            format!(
                "DoT connection to {endpoint} failed: {}",
                last_error.map_or_else(|| "no address".to_string(), |error| error.to_string())
            )
        })?;
        tcp.set_read_timeout(Some(timeout))
            .map_err(|error| format!("DoT read timeout: {error}"))?;
        tcp.set_write_timeout(Some(timeout))
            .map_err(|error| format!("DoT write timeout: {error}"))?;
        let name = ServerName::try_from(server_name.to_owned())
            .map_err(|_| "DoT TLS server name is invalid".to_string())?;
        let connection = ClientConnection::new(Arc::clone(&self.tls), name)
            .map_err(|error| format!("DoT TLS connection failed: {error}"))?;
        self.stream = Some(StreamOwned::new(connection, tcp));
        self.connected_endpoint = endpoint.to_string();
        self.connected_server_name = server_name.to_string();
        Ok(())
    }

    fn query_connected(&mut self, dns_query: &[u8]) -> Result<Vec<u8>, String> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| "DoT connection is unavailable".to_string())?;
        stream
            .write_all(&(dns_query.len() as u16).to_be_bytes())
            .and_then(|_| stream.write_all(dns_query))
            .and_then(|_| stream.flush())
            .map_err(|error| format!("DoT write failed: {error}"))?;
        let mut length = [0u8; 2];
        stream
            .read_exact(&mut length)
            .map_err(|error| format!("DoT response length failed: {error}"))?;
        let length = u16::from_be_bytes(length) as usize;
        if length < 12 {
            return Err("DoT returned a truncated DNS response".to_string());
        }
        let mut response = vec![0; length];
        stream
            .read_exact(&mut response)
            .map_err(|error| format!("DoT response failed: {error}"))?;
        Ok(response)
    }
}

#[cfg(test)]
mod agent_tests {
    use super::*;

    #[test]
    fn build_agent_accepts_supported_timeout() {
        assert!(build_agent(2500).is_ok());
    }

    #[test]
    fn checkout_reuses_agent_within_idle_window() {
        let t0 = Instant::now();
        let mut shared = SharedAgent::new_at(3000, t0);
        shared.checkout_at(t0 + Duration::from_secs(1)).unwrap();
        let generation = shared.generation;
        shared
            .checkout_at(t0 + Duration::from_secs(1) + AGENT_IDLE_MAX)
            .unwrap();
        assert_eq!(
            shared.generation, generation,
            "client must be reused when idle <= 15 s"
        );
    }

    #[test]
    fn checkout_rebuilds_agent_after_idle_window() {
        let t0 = Instant::now();
        let mut shared = SharedAgent::new_at(3000, t0);
        shared.checkout_at(t0).unwrap();
        let first_generation = shared.generation;
        let t1 = t0 + AGENT_IDLE_MAX + Duration::from_millis(1);
        shared.checkout_at(t1).unwrap();
        assert_eq!(shared.generation, first_generation + 1);
        // The rebuild restarts the idle clock.
        shared.checkout_at(t1 + Duration::from_secs(10)).unwrap();
        assert_eq!(shared.generation, first_generation + 1);
    }

    #[test]
    fn checkout_tolerates_clock_going_backwards() {
        let t0 = Instant::now();
        let mut shared = SharedAgent::new_at(3000, t0 + Duration::from_secs(60));
        shared.checkout_at(t0 + Duration::from_secs(60)).unwrap();
        let generation = shared.generation;
        shared.checkout_at(t0).unwrap();
        assert_eq!(shared.generation, generation);
    }

    #[test]
    fn set_timeout_rebuilds_only_on_change() {
        let t0 = Instant::now();
        let mut shared = SharedAgent::new_at(3000, t0);
        shared.checkout_at(t0).unwrap();
        let first_generation = shared.generation;
        shared.set_timeout(3000);
        shared.checkout_at(t0).unwrap();
        assert_eq!(shared.generation, first_generation);
        shared.set_timeout(5000);
        shared.checkout_at(t0).unwrap();
        assert_eq!(shared.generation, first_generation + 1);
    }

    #[test]
    fn reset_rebuilds_the_http_client_even_when_it_was_recently_used() {
        let t0 = Instant::now();
        let mut shared = SharedAgent::new_at(3000, t0);
        shared.checkout_at(t0).unwrap();
        let first_generation = shared.generation;
        shared.reset();
        shared.checkout_at(t0 + Duration::from_millis(1)).unwrap();
        assert_eq!(shared.generation, first_generation + 1);
    }
}
