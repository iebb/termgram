// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use log::info;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
pub use tokio::net::tcp::{ReadHalf, WriteHalf};

use super::ServerAddr;

pub enum NetStream {
    Tcp(TcpStream),
    #[cfg(feature = "proxy")]
    ProxySocks5(tokio_socks::tcp::Socks5Stream<TcpStream>),
    #[cfg(feature = "proxy")]
    ProxyHttp(TcpStream),
}

impl NetStream {
    pub(crate) fn split(&mut self) -> (ReadHalf<'_>, WriteHalf<'_>) {
        match self {
            Self::Tcp(stream) => stream.split(),
            #[cfg(feature = "proxy")]
            Self::ProxySocks5(stream) => stream.split(),
            #[cfg(feature = "proxy")]
            Self::ProxyHttp(stream) => stream.split(),
        }
    }

    pub(crate) async fn connect(addr: &ServerAddr) -> Result<Self, std::io::Error> {
        info!("connecting...");
        match addr {
            ServerAddr::Tcp { address } => Ok(NetStream::Tcp(TcpStream::connect(address).await?)),
            #[cfg(feature = "proxy")]
            ServerAddr::Proxied { address, proxy } => {
                Self::connect_proxy_stream(address, proxy).await
            }
        }
    }

    #[cfg(feature = "proxy")]
    async fn connect_proxy_stream(
        addr: &std::net::SocketAddr,
        proxy_url: &str,
    ) -> Result<NetStream, std::io::Error> {
        use std::{
            io::{self, ErrorKind},
            net::{IpAddr, SocketAddr},
        };

        use hickory_resolver::Resolver;
        use url::Host;

        let proxy = url::Url::parse(proxy_url)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
        let scheme = proxy.scheme();
        let host = proxy.host().ok_or(io::Error::new(
            ErrorKind::NotFound,
            format!("proxy host is missing from url: {}", proxy_url),
        ))?;
        let port = proxy.port().ok_or(io::Error::new(
            ErrorKind::NotFound,
            format!("proxy port is missing from url: {}", proxy_url),
        ))?;
        let username = proxy.username();
        let password = proxy.password().unwrap_or("");
        let socks_addr = match host {
            Host::Domain(domain) => {
                let resolver = Resolver::builder_tokio().unwrap().build().unwrap();
                let response = resolver
                    .lookup_ip(domain)
                    .await
                    .map_err(|err| io::Error::new(ErrorKind::Other, err))?;
                let socks_ip_addr = response.iter().next().ok_or(io::Error::new(
                    ErrorKind::NotFound,
                    format!("proxy host did not return any ip address: {}", domain),
                ))?;
                SocketAddr::new(socks_ip_addr, port)
            }
            Host::Ipv4(v4) => SocketAddr::new(IpAddr::from(v4), port),
            Host::Ipv6(v6) => SocketAddr::new(IpAddr::from(v6), port),
        };

        match scheme {
            "socks5" => {
                if username.is_empty() {
                    Ok(NetStream::ProxySocks5(
                        tokio_socks::tcp::Socks5Stream::connect(socks_addr, addr)
                            .await
                            .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?,
                    ))
                } else {
                    Ok(NetStream::ProxySocks5(
                        tokio_socks::tcp::Socks5Stream::connect_with_password(
                            socks_addr, addr, username, password,
                        )
                        .await
                        .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?,
                    ))
                }
            }
            "http" => {
                let mut stream = TcpStream::connect(socks_addr).await?;
                http_connect_tunnel(&mut stream, addr, username, password).await?;
                Ok(NetStream::ProxyHttp(stream))
            }
            scheme => Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                format!("proxy scheme not supported: {}", scheme),
            )),
        }
    }

    pub(crate) async fn disconnect(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown().await,
            #[cfg(feature = "proxy")]
            Self::ProxySocks5(stream) => stream.shutdown().await,
            #[cfg(feature = "proxy")]
            Self::ProxyHttp(stream) => stream.shutdown().await,
        }
    }
}

/// Negotiate an HTTP CONNECT tunnel through `stream` to `addr`, optionally
/// authenticating with the proxy. On success the stream is a raw tunnel.
#[cfg(feature = "proxy")]
async fn http_connect_tunnel(
    stream: &mut TcpStream,
    addr: &std::net::SocketAddr,
    username: &str,
    password: &str,
) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;

    let mut request = format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n");
    if !username.is_empty() {
        let credentials = base64_basic_auth(username, password);
        request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut response = [0u8; 1024];
    let mut received = 0;
    let end = loop {
        if received == response.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "proxy response exceeds the CONNECT handshake buffer",
            ));
        }
        let read = stream.read(&mut response[received..]).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "proxy closed the connection during the CONNECT handshake",
            ));
        }
        received += read;
        if let Some(end) = find_header_end(&response[..received]) {
            break end;
        }
    };
    parse_connect_response(&response[..end])
}

/// Return the offset just past the empty line ending an HTTP header block.
#[cfg(feature = "proxy")]
fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// Validate a CONNECT response status line, ignoring its headers.
#[cfg(feature = "proxy")]
fn parse_connect_response(response: &[u8]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let head = std::str::from_utf8(response)
        .map_err(|_| Error::new(ErrorKind::InvalidData, "proxy response is not UTF-8"))?;
    let status = head.lines().next().unwrap_or_default();
    let mut parts = status.split(' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default().parse::<u16>().unwrap_or(0);
    if matches!(version, "HTTP/1.0" | "HTTP/1.1") && (200..300).contains(&code) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::ConnectionAborted,
            format!("proxy CONNECT failed: {status}"),
        ))
    }
}

/// Minimal standard base64 encoder for proxy basic authentication.
#[cfg(feature = "proxy")]
fn base64_basic_auth(username: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut credentials = username.as_bytes().to_vec();
    credentials.push(b':');
    credentials.extend_from_slice(password.as_bytes());
    let mut encoded = String::with_capacity(credentials.len().div_ceil(3) * 4);
    for chunk in credentials.chunks(3) {
        let group = u32::from_be_bytes([
            0,
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ]);
        encoded.push(TABLE[(group >> 18) as usize & 0x3f] as char);
        encoded.push(TABLE[(group >> 12) as usize & 0x3f] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(group >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[group as usize & 0x3f] as char
        } else {
            '='
        });
    }
    encoded
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "proxy")]
    fn connect_response_parsing_accepts_success_and_rejects_other_statuses() {
        use super::{find_header_end, parse_connect_response};

        let ok = b"HTTP/1.1 200 Connection established\r\nProxy: x\r\n\r\nbody";
        let end = find_header_end(ok).expect("header end");
        assert_eq!(&ok[..end], b"HTTP/1.1 200 Connection established\r\nProxy: x\r\n\r\n");
        assert!(parse_connect_response(&ok[..end]).is_ok());

        assert!(parse_connect_response(b"HTTP/1.0 200\r\n\r\n").is_ok());
        for rejected in [
            &b"HTTP/1.1 403 Forbidden\r\n\r\n"[..],
            b"HTTP/2 200\r\n\r\n",
            b"garbage\r\n\r\n",
            b"",
        ] {
            assert!(parse_connect_response(rejected).is_err(), "{rejected:?}");
        }
        assert_eq!(find_header_end(b"HTTP/1.1 200\r\n"), None);
    }

    #[test]
    #[cfg(feature = "proxy")]
    fn basic_auth_encoding_matches_reference_vectors() {
        use super::base64_basic_auth;

        assert_eq!(base64_basic_auth("user", "pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_basic_auth("u", ""), "dTo=");
        assert_eq!(base64_basic_auth("", ""), "Og==");
        assert_eq!(base64_basic_auth("us", "p"), "dXM6cA==");
    }
}
