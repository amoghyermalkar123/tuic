use self::connection::Connection;
use quinn::{ClientConfig, ConnectionError, Endpoint, SendDatagramError, WriteError};
use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    io::Error as IoError,
    net::SocketAddr,
};
use thiserror::Error;
use tokio::{
    net::lookup_host,
    sync::mpsc::{self, Receiver, Sender},
};
use tuic_protocol::Error as ProtocolError;

pub use self::{address::Address, request::Request};

mod address;
mod connection;
mod request;

pub struct Relay {
    req_rx: Receiver<Request>,
    endpoint: Endpoint,
    server_addr: ServerAddr,
    token_digest: [u8; 32],
    udp_mode: UdpMode,
    reduce_rtt: bool,
}

impl Relay {
    pub fn init(
        config: ClientConfig,
        server_addr: ServerAddr,
        token_digest: [u8; 32],
        udp_mode: UdpMode,
        reduce_rtt: bool,
    ) -> Result<(Self, Sender<Request>), IoError> {
        let mut endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?;
        endpoint.set_default_client_config(config);

        let (req_tx, req_rx) = mpsc::channel(1);

        let relay = Self {
            req_rx,
            endpoint,
            server_addr,
            token_digest,
            udp_mode,
            reduce_rtt,
        };

        Ok((relay, req_tx))
    }

    pub async fn run(mut self) {
        log::info!("[relay] started. Target server: {}", self.server_addr);

        let mut conn = self.establish_connection().await;
        log::debug!("[relay] [connection] [establish]");

        while let Some(req) = self.req_rx.recv().await {
            if conn.is_closed() {
                log::debug!("[relay] [connection] [disconnect]");
                conn = self.establish_connection().await;
                log::debug!("[relay] [connection] [establish]");
            }

            let conn_cloned = conn.clone();

            tokio::spawn(async move {
                match conn_cloned.process_request(req).await {
                    Ok(()) => (),
                    Err(err) => {
                        log::warn!("[relay] [task] {err}");
                    }
                }
            });
        }
    }

    async fn establish_connection(&self) -> Connection {
        let (mut addrs, server_name) = match &self.server_addr {
            ServerAddr::HostnameAddr { hostname, .. } => (Vec::new(), hostname),
            ServerAddr::SocketAddr {
                server_addr,
                server_name,
            } => (vec![*server_addr], server_name),
        };

        loop {
            if let ServerAddr::HostnameAddr {
                hostname,
                server_port,
            } = &self.server_addr
            {
                match lookup_host((hostname.as_str(), *server_port)).await {
                    Ok(resolved) => addrs = resolved.collect(),
                    Err(err) => {
                        log::error!("[relay] [connection] {err}");
                        continue;
                    }
                }
            }

            for addr in &addrs {
                match self.endpoint.connect(*addr, server_name) {
                    Ok(conn) => {
                        match Connection::init(
                            conn,
                            self.token_digest,
                            self.udp_mode,
                            self.reduce_rtt,
                        )
                        .await
                        {
                            Ok(conn) => return conn,
                            Err(err) => log::error!("[relay] [connection] {err}"),
                        }
                    }
                    Err(err) => log::error!("[relay] [connection] {err}"),
                }
            }
        }
    }
}

pub enum ServerAddr {
    SocketAddr {
        server_addr: SocketAddr,
        server_name: String,
    },
    HostnameAddr {
        hostname: String,
        server_port: u16,
    },
}

impl Display for ServerAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            ServerAddr::SocketAddr {
                server_addr,
                server_name,
            } => write!(f, "{server_addr} ({server_name})"),
            ServerAddr::HostnameAddr {
                hostname,
                server_port,
            } => write!(f, "{hostname}:{server_port}"),
        }
    }
}

#[derive(Clone, Copy)]
pub enum UdpMode {
    Native,
    Quic,
}

#[derive(Debug, Error)]
pub enum RelayError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Io(#[from] IoError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    WriteStream(#[from] WriteError),
    #[error(transparent)]
    SendDatagram(#[from] SendDatagramError),
    #[error("UDP session not found: {0}")]
    UdpSessionNotFound(u32),
    #[error("bad command")]
    BadCommand,
}
