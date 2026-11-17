//! Pure-Rust Protobuf wire codec and HTTP/2 gRPC unary/server-streaming client
//! used by the native Talos Linux (`MachineService` & COSI `State`) controller.

use anyhow::{anyhow, bail, Context, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{HeaderMap, HeaderValue, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::sync::Arc;
use tokio_rustls::TlsConnector;

use crate::transport::{open_raw_or_tunneled_tcp, server_name_for_host};

/// Decoded Protobuf wire field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireValue {
    Varint(u64),
    Fixed64(u64),
    Bytes(Vec<u8>),
    Fixed32(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireField {
    pub number: u32,
    pub value: WireValue,
}

impl WireField {
    pub fn as_varint(&self) -> Option<u64> {
        match self.value {
            WireValue::Varint(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.value {
            WireValue::Bytes(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }
}

pub fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        if (val & !0x7F) == 0 {
            out.push(val as u8);
            return;
        }
        out.push(((val & 0x7F) as u8) | 0x80);
        val >>= 7;
    }
}

pub fn encode_tag(field_number: u32, wire_type: u8, out: &mut Vec<u8>) {
    encode_varint(((field_number as u64) << 3) | (wire_type as u64), out);
}

pub fn encode_varint_field(field_number: u32, val: u64, out: &mut Vec<u8>) {
    if val == 0 {
        return;
    }
    encode_tag(field_number, 0, out);
    encode_varint(val, out);
}

pub fn encode_bool_field(field_number: u32, val: bool, out: &mut Vec<u8>) {
    if val {
        encode_varint_field(field_number, 1, out);
    }
}

pub fn encode_bytes_field(field_number: u32, data: &[u8], out: &mut Vec<u8>) {
    if data.is_empty() {
        return;
    }
    encode_tag(field_number, 2, out);
    encode_varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

pub fn encode_string_field(field_number: u32, s: &str, out: &mut Vec<u8>) {
    encode_bytes_field(field_number, s.as_bytes(), out);
}

pub fn decode_varint(buf: &mut &[u8]) -> Result<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    while !buf.is_empty() {
        let byte = buf[0];
        *buf = &buf[1..];
        result |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            bail!("protobuf varint overflow");
        }
    }
    bail!("unexpected EOF decoding protobuf varint")
}

pub fn decode_fields(mut input: &[u8]) -> Result<Vec<WireField>> {
    let mut fields = Vec::new();
    while !input.is_empty() {
        let tag = decode_varint(&mut input)?;
        let field_number = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u8;
        if field_number == 0 {
            bail!("invalid protobuf field number 0");
        }
        let value = match wire_type {
            0 => WireValue::Varint(decode_varint(&mut input)?),
            1 => {
                if input.len() < 8 {
                    bail!("unexpected EOF in fixed64");
                }
                let v = u64::from_le_bytes(input[..8].try_into().unwrap());
                input = &input[8..];
                WireValue::Fixed64(v)
            }
            2 => {
                let len = decode_varint(&mut input)? as usize;
                if input.len() < len {
                    bail!("unexpected EOF in length-delimited field (need {len}, have {})", input.len());
                }
                let slice = input[..len].to_vec();
                input = &input[len..];
                WireValue::Bytes(slice)
            }
            5 => {
                if input.len() < 4 {
                    bail!("unexpected EOF in fixed32");
                }
                let v = u32::from_le_bytes(input[..4].try_into().unwrap());
                input = &input[4..];
                WireValue::Fixed32(v)
            }
            other => bail!("unsupported protobuf wire type {other}"),
        };
        fields.push(WireField {
            number: field_number,
            value,
        });
    }
    Ok(fields)
}

/// Wraps a serialized Protobuf message into an uncompressed 5-byte gRPC frame.
pub fn frame_grpc_message(proto_bytes: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + proto_bytes.len());
    buf.put_u8(0); // uncompressed flag
    buf.put_u32(proto_bytes.len() as u32);
    buf.put_slice(proto_bytes);
    buf.freeze()
}

/// Unpacks one or more gRPC length-prefixed frames from a response body buffer.
pub fn unframe_grpc_messages(mut data: Bytes) -> Result<Vec<Vec<u8>>> {
    let mut msgs = Vec::new();
    while data.has_remaining() {
        if data.remaining() < 5 {
            bail!("truncated gRPC frame header ({} bytes)", data.remaining());
        }
        let compressed = data.get_u8();
        if compressed != 0 {
            bail!("compressed gRPC frames (flag={compressed}) are not requested or supported");
        }
        let len = data.get_u32() as usize;
        if data.remaining() < len {
            bail!(
                "truncated gRPC frame body: expected {} bytes, got {}",
                len,
                data.remaining()
            );
        }
        let chunk = data.copy_to_bytes(len).to_vec();
        msgs.push(chunk);
    }
    Ok(msgs)
}

/// Connection configuration for a gRPC mTLS call (optionally tunneled via `pits-agent`).
#[derive(Debug, Clone)]
pub struct GrpcTarget {
    pub host: String,
    pub port: u16,
    pub tls_server_name: Option<String>,
    pub tls_config: Arc<rustls::ClientConfig>,
    pub bridge_proxy_url: Option<String>,
    pub bridge_bearer_token: Option<String>,
    pub bridge_tls_config: Option<Arc<rustls::ClientConfig>>,
}

/// Invokes a unary or server-streaming gRPC method over HTTP/2 + mTLS and returns all response messages.
pub async fn invoke_grpc(
    target: &GrpcTarget,
    service_path: &str,
    node_metadata: Option<&str>,
    request_proto: &[u8],
) -> Result<Vec<Vec<u8>>> {
    let tcp = open_raw_or_tunneled_tcp(
        &target.host,
        target.port,
        target.bridge_proxy_url.as_deref(),
        target.bridge_bearer_token.as_deref(),
        target.bridge_tls_config.clone(),
    )
    .await?;

    let sni_host = target
        .tls_server_name
        .as_deref()
        .unwrap_or(target.host.as_str());
    let server_name = server_name_for_host(sni_host)?;
    let connector = TlsConnector::from(target.tls_config.clone());
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .with_context(|| format!("gRPC TLS handshake to {}:{}", target.host, target.port))?;

    let (mut sender, conn) = http2::handshake(TokioExecutor::new(), TokioIo::new(tls_stream))
        .await
        .context("gRPC HTTP/2 handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let uri: Uri = format!("https://{}:{}{}", target.host, target.port, service_path)
        .parse()
        .context("invalid gRPC URI")?;

    let mut req_builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("user-agent", "pitsctl-rust/1.0");
    if let Some(node) = node_metadata {
        if !node.is_empty() {
            req_builder = req_builder.header("node", HeaderValue::from_str(node)?);
        }
    }
    let req = req_builder
        .body(Full::new(frame_grpc_message(request_proto)))
        .context("build gRPC HTTP/2 request")?;

    let resp = sender.send_request(req).await.context("send gRPC request")?;
    let status = resp.status();
    let initial_headers = resp.headers().clone();

    let mut body = resp.into_body();
    let mut payload = BytesMut::new();
    let mut trailers = HeaderMap::new();

    while let Some(frame_res) = body.frame().await {
        let frame = frame_res.context("read gRPC HTTP/2 frame")?;
        match frame.into_data() {
            Ok(data) => payload.extend_from_slice(&data),
            Err(other_frame) => {
                if let Ok(tr) = other_frame.into_trailers() {
                    trailers.extend(tr);
                }
            }
        }
    }

    if !status.is_success() {
        bail!("gRPC HTTP status {} on {}", status, service_path);
    }
    check_grpc_status(&initial_headers)?;
    check_grpc_status(&trailers)?;

    unframe_grpc_messages(payload.freeze())
}

fn check_grpc_status(headers: &HeaderMap) -> Result<()> {
    if let Some(code_val) = headers.get("grpc-status") {
        let code_str = code_val.to_str().unwrap_or("0");
        if code_str != "0" {
            let msg = headers
                .get("grpc-message")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown gRPC error");
            return Err(anyhow!("gRPC error code {code_str}: {msg}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protobuf_wire_roundtrip_and_grpc_framing() {
        let mut buf = Vec::new();
        encode_string_field(1, "netboot-004", &mut buf);
        encode_varint_field(2, 4420, &mut buf);
        encode_bool_field(3, true, &mut buf);

        let framed = frame_grpc_message(&buf);
        let unframed = unframe_grpc_messages(framed).unwrap();
        assert_eq!(unframed.len(), 1);

        let fields = decode_fields(&unframed[0]).unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].as_str(), Some("netboot-004"));
        assert_eq!(fields[1].as_varint(), Some(4420));
        assert_eq!(fields[2].as_varint(), Some(1));
    }
}
