//! Pure-Rust mTLS transport, HTTP CONNECT Lab VLAN 20 tunnel dialer,
//! multi-cluster Kubernetes REST client, and Go `pits-agent` RPC client.

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use http::{Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::BufReader;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::config::{normalize_talos_ed25519_key_pem, ClientProfile, ResolvedKubeEndpoint};

/// Trait-object async stream wrapper so plain TCP and TLS-over-TCP streams share one type
/// (for instance when `pits-agent` itself runs over mTLS and serves `HTTP CONNECT` tunnels).
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

pub struct BoxedStream(pub Box<dyn AsyncReadWrite>);

impl AsyncRead for BoxedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

/// Custom certificate verifier used only when `insecure-skip-tls-verify: true` or
/// when inspecting a self-signed Windows RDP TLS certificate to capture its thumbprint.
#[derive(Debug)]
pub struct RecordingCertVerifier {
    pub captured_leaf_der: Arc<Mutex<Option<Vec<u8>>>>,
    pub provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for RecordingCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if let Ok(mut guard) = self.captured_leaf_der.lock() {
            *guard = Some(end_entity.as_ref().to_vec());
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Builds a pure-Rust (`rustls-rustcrypto`) TLS 1.2/1.3 client configuration supporting
/// Ed25519, ECDSA, and RSA certificates + optional mTLS client identity + ALPN.
pub fn build_rustls_client_config(
    ca_pem: Option<&[u8]>,
    client_cert_pem: Option<&[u8]>,
    client_key_pem: Option<&[u8]>,
    insecure_skip_verify: bool,
    alpn_protocols: Vec<Vec<u8>>,
) -> Result<(Arc<rustls::ClientConfig>, Arc<Mutex<Option<Vec<u8>>>>)> {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let captured_leaf = Arc::new(Mutex::new(None));

    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("rustls protocol versions: {e}"))?;

    let mut cfg = if insecure_skip_verify || ca_pem.is_none() {
        let verifier = Arc::new(RecordingCertVerifier {
            captured_leaf_der: captured_leaf.clone(),
            provider: provider.clone(),
        });
        let b = builder.dangerous().with_custom_certificate_verifier(verifier);
        attach_client_auth_dangerous(b, client_cert_pem, client_key_pem)?
    } else {
        let mut roots = RootCertStore::empty();
        let mut reader = BufReader::new(ca_pem.unwrap());
        for cert in rustls_pemfile::certs(&mut reader) {
            roots
                .add(cert.context("parse CA PEM certificate")?)
                .map_err(|e| anyhow!("add CA certificate to root store: {e}"))?;
        }
        let b = builder.with_root_certificates(roots);
        attach_client_auth_verified(b, client_cert_pem, client_key_pem)?
    };

    cfg.alpn_protocols = alpn_protocols;
    Ok((Arc::new(cfg), captured_leaf))
}

fn parse_client_identity(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let mut cert_reader = BufReader::new(cert_pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parse client certificate PEM")?;
    if certs.is_empty() {
        bail!("client certificate PEM contained zero certificates");
    }

    let normalized_key = normalize_talos_ed25519_key_pem(key_pem)?;
    let mut key_reader = BufReader::new(normalized_key.as_slice());
    let key = rustls_pemfile::private_key(&mut key_reader)
        .context("parse client private key PEM")?
        .ok_or_else(|| anyhow!("no private key found in client key PEM"))?;
    Ok((certs, key))
}

fn attach_client_auth_verified(
    builder: rustls::ConfigBuilder<rustls::ClientConfig, rustls::client::WantsClientCert>,
    client_cert_pem: Option<&[u8]>,
    client_key_pem: Option<&[u8]>,
) -> Result<rustls::ClientConfig> {
    match (client_cert_pem, client_key_pem) {
        (Some(c), Some(k)) => {
            let (certs, key) = parse_client_identity(c, k)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| anyhow!("configure mTLS client certificate: {e}"))
        }
        _ => Ok(builder.with_no_client_auth()),
    }
}

fn attach_client_auth_dangerous(
    builder: rustls::ConfigBuilder<rustls::ClientConfig, rustls::client::WantsClientCert>,
    client_cert_pem: Option<&[u8]>,
    client_key_pem: Option<&[u8]>,
) -> Result<rustls::ClientConfig> {
    attach_client_auth_verified(builder, client_cert_pem, client_key_pem)
}

pub fn server_name_for_host(host: &str) -> Result<ServerName<'static>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        Ok(ServerName::IpAddress(ip.into()))
    } else {
        ServerName::try_from(host.to_string())
            .map_err(|e| anyhow!("invalid TLS ServerName {host:?}: {e}"))
    }
}

/// Opens either a direct TCP connection to `target_host:target_port`, or tunnels through
/// `pits-agent` via `HTTP CONNECT` (`CONNECT <target_host>:<target_port> HTTP/1.1`).
pub async fn open_raw_or_tunneled_tcp(
    target_host: &str,
    target_port: u16,
    bridge_proxy_url: Option<&str>,
    bridge_bearer_token: Option<&str>,
    bridge_tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<BoxedStream> {
    let Some(proxy_raw) = bridge_proxy_url.filter(|s| !s.trim().is_empty()) else {
        let tcp = TcpStream::connect((target_host, target_port))
            .await
            .with_context(|| format!("connect TCP {target_host}:{target_port}"))?;
        let _ = tcp.set_nodelay(true);
        return Ok(BoxedStream(Box::new(tcp)));
    };

    let proxy_url = Url::parse(proxy_raw).with_context(|| format!("parse bridge URL {proxy_raw}"))?;
    let proxy_host = proxy_url
        .host_str()
        .ok_or_else(|| anyhow!("bridge URL missing host"))?;
    let proxy_port = proxy_url.port_or_known_default().unwrap_or(18443);

    let tcp = TcpStream::connect((proxy_host, proxy_port))
        .await
        .with_context(|| format!("connect to pits-agent bridge {proxy_host}:{proxy_port}"))?;
    let _ = tcp.set_nodelay(true);

    let mut stream: BoxedStream = if proxy_url.scheme() == "https" {
        let tls_cfg = match bridge_tls_config {
            Some(c) => c,
            None => build_rustls_client_config(None, None, None, true, vec![b"http/1.1".to_vec()])?.0,
        };
        let connector = TlsConnector::from(tls_cfg);
        let tls = connector
            .connect(server_name_for_host(proxy_host)?, tcp)
            .await
            .context("TLS handshake to pits-agent bridge")?;
        BoxedStream(Box::new(tls))
    } else {
        BoxedStream(Box::new(tcp))
    };

    let mut connect_req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
    );
    if let Some(tok) = bridge_bearer_token.filter(|t| !t.is_empty()) {
        connect_req.push_str(&format!("Authorization: Bearer {tok}\r\n"));
    }
    connect_req.push_str("\r\n");

    stream
        .write_all(connect_req.as_bytes())
        .await
        .context("send HTTP CONNECT to pits-agent bridge")?;
    stream.flush().await?;

    // Read response header byte-by-byte up to \r\n\r\n so zero tunneled payload bytes are over-read.
    let mut hdr = Vec::with_capacity(256);
    let mut one = [0u8; 1];
    while !hdr.ends_with(b"\r\n\r\n") {
        if hdr.len() > 8192 {
            bail!("HTTP CONNECT response header exceeded 8 KiB");
        }
        let n = stream.read(&mut one).await?;
        if n == 0 {
            bail!("EOF from pits-agent bridge before HTTP CONNECT tunnel established");
        }
        hdr.push(one[0]);
    }
    let header_text = String::from_utf8_lossy(&hdr);
    let status_line = header_text.lines().next().unwrap_or("");
    if !status_line.contains(" 200 ") {
        bail!(
            "pits-agent HTTP CONNECT tunnel rejected for {target_host}:{target_port}: {status_line}"
        );
    }

    Ok(stream)
}

/// Performs an HTTP/1.1 request over plain TCP or TLS, optionally tunneled through `pits-agent`.
#[allow(clippy::too_many_arguments)]
pub async fn send_http_request(
    method: Method,
    full_url: &str,
    content_type: Option<&str>,
    bearer_token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body_bytes: Option<Vec<u8>>,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    tls_server_name: Option<&str>,
    bridge_proxy_url: Option<&str>,
    bridge_bearer_token: Option<&str>,
    bridge_tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<(u16, Bytes)> {
    let parsed = Url::parse(full_url).with_context(|| format!("invalid URL {full_url}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("URL missing host: {full_url}"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("URL missing port: {full_url}"))?;

    let raw_stream = open_raw_or_tunneled_tcp(
        host,
        port,
        bridge_proxy_url,
        bridge_bearer_token,
        bridge_tls_config,
    )
    .await?;

    let io_stream: BoxedStream = if parsed.scheme() == "https" {
        let cfg = match tls_config {
            Some(c) => c,
            None => build_rustls_client_config(None, None, None, true, vec![b"http/1.1".to_vec()])?.0,
        };
        let connector = TlsConnector::from(cfg);
        let sni = tls_server_name.filter(|s| !s.is_empty()).unwrap_or(host);
        let tls = connector
            .connect(server_name_for_host(sni)?, raw_stream)
            .await
            .with_context(|| format!("TLS handshake to {host}:{port} (sni={sni})"))?;
        BoxedStream(Box::new(tls))
    } else {
        raw_stream
    };

    let (mut sender, conn) = http1::handshake(TokioIo::new(io_stream))
        .await
        .context("HTTP/1.1 handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let path_and_query = match parsed.query() {
        Some(q) => format!("{}?{}", parsed.path(), q),
        None => parsed.path().to_string(),
    };
    let uri: Uri = path_and_query.parse().context("parse request path URI")?;
    let host_header = match parsed.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    };

    let mut req_builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Host", host_header)
        .header("User-Agent", "pitsctl-rust/1.0");
    if !extra_headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("Accept"))
    {
        req_builder = req_builder.header("Accept", "application/json");
    }
    if let Some(ct) = content_type {
        req_builder = req_builder.header("Content-Type", ct);
    }
    if let Some(tok) = bearer_token.filter(|t| !t.is_empty()) {
        req_builder = req_builder.header("Authorization", format!("Bearer {tok}"));
    }
    for (k, v) in extra_headers {
        req_builder = req_builder.header(*k, *v);
    }

    let payload = Bytes::from(body_bytes.unwrap_or_default());
    let req = req_builder
        .body(Full::new(payload))
        .context("build HTTP request")?;
    let resp = sender.send_request(req).await.context("send HTTP request")?;
    let code = resp.status().as_u16();
    let collected = resp
        .into_body()
        .collect()
        .await
        .context("read HTTP response body")?
        .to_bytes();
    Ok((code, collected))
}

/// Native multi-cluster Kubernetes API client (`core` RKE2 & `lab` Talos K8s).
#[derive(Clone)]
pub struct KubeClient {
    pub cluster_label: String,
    pub endpoint: ResolvedKubeEndpoint,
    pub tls_config: Arc<rustls::ClientConfig>,
    pub bridge_proxy_url: Option<String>,
    pub bridge_bearer_token: Option<String>,
    pub bridge_tls_config: Option<Arc<rustls::ClientConfig>>,
}

impl KubeClient {
    pub fn from_endpoint(
        cluster_label: &str,
        endpoint: ResolvedKubeEndpoint,
        bridge: Option<(&ClientProfile, Arc<rustls::ClientConfig>)>,
    ) -> Result<Self> {
        let (tls_config, _) = build_rustls_client_config(
            endpoint.ca_pem.as_deref(),
            endpoint.client_cert_pem.as_deref(),
            endpoint.client_key_pem.as_deref(),
            endpoint.insecure_skip_tls_verify,
            vec![b"http/1.1".to_vec()],
        )?;

        let (bridge_proxy_url, bridge_bearer_token, bridge_tls_config) = match bridge {
            Some((prof, b_tls)) => (
                Some(prof.agent_endpoint.clone()),
                prof.agent_bearer_token.clone(),
                Some(b_tls),
            ),
            None => (None, None, None),
        };

        Ok(Self {
            cluster_label: cluster_label.to_string(),
            endpoint,
            tls_config,
            bridge_proxy_url,
            bridge_bearer_token,
            bridge_tls_config,
        })
    }

    pub async fn get_json<T: DeserializeOwned>(&self, api_path: &str) -> Result<T> {
        self.get_json_with_headers(api_path, &[]).await
    }

    pub async fn get_json_with_headers<T: DeserializeOwned>(
        &self,
        api_path: &str,
        extra_headers: &[(&str, &str)],
    ) -> Result<T> {
        let (code, body) = self
            .raw_request(Method::GET, api_path, None, extra_headers, None)
            .await?;
        if !(200..300).contains(&code) {
            bail!(
                "[{}] GET {} failed (HTTP {}): {}",
                self.cluster_label,
                api_path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        serde_json::from_slice(&body)
            .with_context(|| format!("[{}] decode JSON from {}", self.cluster_label, api_path))
    }

    /// Server-Side Apply (`PATCH` with `application/apply-patch+yaml`) — deterministic & idempotent!
    pub async fn server_side_apply(
        &self,
        api_path: &str,
        field_manager: &str,
        manifest_json_or_yaml: &[u8],
        force: bool,
    ) -> Result<serde_json::Value> {
        let sep = if api_path.contains('?') { '&' } else { '?' };
        let path = format!("{api_path}{sep}fieldManager={field_manager}&force={force}");
        let (code, body) = self
            .raw_request(
                Method::PATCH,
                &path,
                Some("application/apply-patch+yaml"),
                &[],
                Some(manifest_json_or_yaml.to_vec()),
            )
            .await?;
        if !(200..300).contains(&code) {
            bail!(
                "[{}] SSA PATCH {} failed (HTTP {}): {}",
                self.cluster_label,
                api_path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        Ok(serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
    }

    pub async fn merge_patch(
        &self,
        api_path: &str,
        patch_body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let bytes = serde_json::to_vec(patch_body)?;
        let (code, body) = self
            .raw_request(
                Method::PATCH,
                api_path,
                Some("application/merge-patch+json"),
                &[],
                Some(bytes),
            )
            .await?;
        if !(200..300).contains(&code) {
            bail!(
                "[{}] PATCH {} failed (HTTP {}): {}",
                self.cluster_label,
                api_path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        Ok(serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
    }

    pub async fn put_subresource(&self, api_path: &str, body_opt: Option<Vec<u8>>) -> Result<()> {
        let ct = body_opt.as_ref().map(|_| "application/json");
        let (code, body) = self
            .raw_request(Method::PUT, api_path, ct, &[], body_opt)
            .await?;
        if !(200..300).contains(&code) {
            bail!(
                "[{}] PUT {} failed (HTTP {}): {}",
                self.cluster_label,
                api_path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    pub async fn delete_resource(&self, api_path: &str) -> Result<()> {
        let (code, body) = self
            .raw_request(Method::DELETE, api_path, None, &[], None)
            .await?;
        if code == 404 || (200..300).contains(&code) {
            return Ok(());
        }
        bail!(
            "[{}] DELETE {} failed (HTTP {}): {}",
            self.cluster_label,
            api_path,
            code,
            String::from_utf8_lossy(&body)
        )
    }

    async fn raw_request(
        &self,
        method: Method,
        api_path: &str,
        content_type: Option<&str>,
        extra_headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Bytes)> {
        let url = format!("{}{}", self.endpoint.server_url, api_path);
        send_http_request(
            method,
            &url,
            content_type,
            self.endpoint.bearer_token.as_deref(),
            extra_headers,
            body,
            Some(self.tls_config.clone()),
            self.endpoint.tls_server_name.as_deref(),
            self.bridge_proxy_url.as_deref(),
            self.bridge_bearer_token.as_deref(),
            self.bridge_tls_config.clone(),
        )
        .await
    }
}

/// Typed client for the Go `pits-agent` privileged node daemon on `pits-core`.
#[derive(Clone)]
pub struct AgentClient {
    pub base_url: String,
    pub bearer_token: Option<String>,
    pub tls_config: Arc<rustls::ClientConfig>,
}

impl AgentClient {
    pub fn from_profile(prof: &ClientProfile) -> Result<Self> {
        let (tls_config, _) = build_rustls_client_config(
            prof.agent_ca_pem.as_deref().map(str::as_bytes),
            prof.agent_client_cert_pem.as_deref().map(str::as_bytes),
            prof.agent_client_key_pem.as_deref().map(str::as_bytes),
            prof.agent_ca_pem.is_none(),
            vec![b"http/1.1".to_vec()],
        )?;
        Ok(Self {
            base_url: prof.agent_endpoint.trim_end_matches('/').to_string(),
            bearer_token: prof.agent_bearer_token.clone(),
            tls_config,
        })
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let (code, body) = send_http_request(
            Method::GET,
            &url,
            None,
            self.bearer_token.as_deref(),
            &[],
            None,
            Some(self.tls_config.clone()),
            None,
            None,
            None,
            None,
        )
        .await?;
        if !(200..300).contains(&code) {
            bail!(
                "pits-agent GET {} returned HTTP {}: {}",
                path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        serde_json::from_slice(&body).with_context(|| format!("decode pits-agent GET {path}"))
    }

    pub async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        req_body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let payload = serde_json::to_vec(req_body)?;
        let (code, body) = send_http_request(
            Method::POST,
            &url,
            Some("application/json"),
            self.bearer_token.as_deref(),
            &[],
            Some(payload),
            Some(self.tls_config.clone()),
            None,
            None,
            None,
            None,
        )
        .await?;
        if !(200..300).contains(&code) {
            bail!(
                "pits-agent POST {} returned HTTP {}: {}",
                path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        serde_json::from_slice(&body).with_context(|| format!("decode pits-agent POST {path}"))
    }

    pub async fn post_raw_json<T: DeserializeOwned>(
        &self,
        path: &str,
        raw_body: Vec<u8>,
        extra_headers: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let (code, body) = send_http_request(
            Method::POST,
            &url,
            Some("application/json"),
            self.bearer_token.as_deref(),
            extra_headers,
            Some(raw_body),
            Some(self.tls_config.clone()),
            None,
            None,
            None,
            None,
        )
        .await?;
        if !(200..300).contains(&code) {
            bail!(
                "pits-agent POST {} returned HTTP {}: {}",
                path,
                code,
                String::from_utf8_lossy(&body)
            );
        }
        serde_json::from_slice(&body).with_context(|| format!("decode pits-agent POST {path}"))
    }
}
