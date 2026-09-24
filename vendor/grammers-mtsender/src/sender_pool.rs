// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::ops::{ControlFlow, Deref};
use std::sync::Arc;
use std::{fmt, panic};

use grammers_mtproto::{mtp, transport};
use grammers_session::types::{DcOption, PeerId, PeerInfo, PeerRef, UpdateState, UpdatesState};
use grammers_session::updates::UpdatesLike;
use grammers_session::{BoxFuture, ErasedSession, Session};
use grammers_tl_types::{self as tl, enums};
use tokio::task::AbortHandle;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};

use crate::configuration::ConnectionParams;
use crate::errors::ReadError;
use crate::{InvocationError, Sender, ServerAddr, connect, connect_with_auth};

pub(crate) type Transport = transport::Full;

type InvokeResponse = Vec<u8>;

/// How long to wait for a proxied connection attempt before treating it as
/// failed and, when fallback is enabled, trying a direct connection.
#[cfg(feature = "proxy")]
const PROXY_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Build the ordered connection attempts for one datacenter address. With the
/// `proxy` feature, a configured proxy comes first and a direct attempt is
/// appended only when [`ConnectionParams::proxy_fallback`] is enabled;
/// otherwise the address is always contacted directly.
fn connection_attempts(address: std::net::SocketAddr, params: &ConnectionParams) -> Vec<ServerAddr> {
    #[cfg(feature = "proxy")]
    match params.proxy_url.as_deref() {
        Some(proxy) => {
            let mut attempts = vec![ServerAddr::Proxied {
                address,
                proxy: proxy.to_owned(),
            }];
            if params.proxy_fallback {
                attempts.push(ServerAddr::Tcp { address });
            }
            attempts
        }
        None => vec![ServerAddr::Tcp { address }],
    }
    #[cfg(not(feature = "proxy"))]
    {
        let _ = params;
        vec![ServerAddr::Tcp { address }]
    }
}

enum Request {
    Invoke {
        dc_id: i32,
        body: Vec<u8>,
        tx: oneshot::Sender<Result<InvokeResponse, InvocationError>>,
    },
    Disconnect {
        dc_id: i32,
    },
    Quit,
}

struct Rpc {
    body: Vec<u8>,
    tx: oneshot::Sender<Result<InvokeResponse, InvocationError>>,
}

struct ConnectionInfo {
    dc_id: i32,
    rpc_tx: mpsc::UnboundedSender<Rpc>,
    abort_handle: AbortHandle,
}

/// A fat [`SenderPoolHandle`] with additional metadata from its attached [`SenderPoolRunner`].
#[derive(Clone)]
pub struct SenderPoolFatHandle {
    /// The inner thin handle that self can be derefed into.
    ///
    /// The rest of fields can be dropped if they are no longer needed.
    pub thin: SenderPoolHandle,
    /// The session in use by the attached [`SenderPoolRunner`].
    ///
    /// The runner will read and persist datacenter options in it.
    pub session: Arc<ErasedSession>,
    /// Developer's [Application Identifier](https://core.telegram.org/myapp).
    ///
    /// The [`SenderPoolRunner`] will make use of this value when it needs
    /// to invoke [`tl::functions::InitConnection`] after creating a new connection.
    pub api_id: i32,
}

/// Cheaply cloneable handle to interact with its [`SenderPoolRunner`].
#[derive(Clone)]
pub struct SenderPoolHandle(mpsc::UnboundedSender<Request>);

/// Builder to configure the runner to drive I/O and linked handles.
pub struct SenderPool {
    /// The single mutable instance responsible for driving I/O.
    ///
    /// Connections are created on-demand, so any errors while the pool
    /// is running can only be retrieved with one of the [`SenderPool::handle`]s.
    pub runner: SenderPoolRunner,
    /// Starting fat handle attached to the [`SenderPool::runner`].
    ///
    /// Handles are the only way to interact with the runner once it's running.
    pub handle: SenderPoolFatHandle,
    /// The single mutable channel through which updates received
    /// from the network by the [`SenderPool::runner`] are delivered.
    ///
    /// Update handling must be processed in a sequential manner,
    /// so this is a separate instance with no way to clone it.
    pub updates: mpsc::UnboundedReceiver<UpdatesLike>,
}

/// Manages and runs a pool of zero or more [`Sender`]s.
///
/// Use [`SenderPool::new`] to create an instance of this type and associated channels.
pub struct SenderPoolRunner {
    session: Arc<ErasedSession>,
    api_id: i32,
    connection_params: ConnectionParams,
    request_rx: mpsc::UnboundedReceiver<Request>,
    updates_tx: mpsc::UnboundedSender<UpdatesLike>,
    connections: Vec<ConnectionInfo>,
    connection_pool: JoinSet<Result<(), ReadError>>,
}

impl Deref for SenderPoolFatHandle {
    type Target = SenderPoolHandle;

    fn deref(&self) -> &Self::Target {
        &self.thin
    }
}

impl SenderPoolHandle {
    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to invoke the serialized request body in the specified datacenter.
    pub async fn invoke_in_dc(
        &self,
        dc_id: i32,
        body: Vec<u8>,
    ) -> Result<InvokeResponse, InvocationError> {
        let (tx, rx) = oneshot::channel();
        self.0
            .send(Request::Invoke { dc_id, body, tx })
            .map_err(|_| InvocationError::Dropped)?;
        rx.await.map_err(|_| InvocationError::Dropped)?
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to drop any active connections to the given datacenter.
    ///
    /// Has no effect if there was no connection to the datacenter.
    ///
    /// This is useful after datacenter migrations during sign in,
    /// when the old connection is known to not be needed anymore.
    pub fn disconnect_from_dc(&self, dc_id: i32) -> bool {
        self.0.send(Request::Disconnect { dc_id }).is_ok()
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to drop all active connections and gracefully stop running.
    pub fn quit(&self) -> bool {
        self.0.send(Request::Quit).is_ok()
    }
}

impl SenderPool {
    /// Creates a new sender pool instance with default configuration,
    /// attached to the given session and using the provided
    /// [Application Identifier](https://core.telegram.org/myapp)
    /// belonging to the developer.
    ///
    /// Session instance **should not** be reused by multiple pools at the same time.
    /// The session instance will only be used to query datacenter options and persist
    /// any permanent Authorization Keys generated for previously-unconncected datacenters.
    pub fn new<S>(session: Arc<S>, api_id: i32) -> Self
    where
        S: Session + Sized,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        Self::with_configuration(session, api_id, Default::default())
    }

    /// Creates a new sender pool with non-[`ConnectionParams::default`] configuration.
    pub fn with_configuration<S>(
        session: Arc<S>,
        api_id: i32,
        connection_params: ConnectionParams,
    ) -> Self
    where
        S: Session + Sized,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let session: Arc<ErasedSession> = Arc::new(Eraser(session));
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (updates_tx, updates_rx) = mpsc::unbounded_channel();

        Self {
            runner: SenderPoolRunner {
                session: Arc::clone(&session),
                api_id,
                connection_params,
                request_rx,
                updates_tx,
                connections: Vec::new(),
                connection_pool: JoinSet::new(),
            },
            handle: SenderPoolFatHandle {
                thin: SenderPoolHandle(request_tx),
                session,
                api_id,
            },
            updates: updates_rx,
        }
    }
}

impl SenderPoolRunner {
    /// Run the sender pool until [`SenderPoolHandle::quit`] is called or the returned future is dropped.
    ///
    /// Connections will be initiated on-demand whenever the first request to a datacenter is made.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                completion = self.connection_pool.join_next(), if !self.connection_pool.is_empty() => {
                    if let Err(err) = completion.unwrap() {
                        if let Ok(reason) = err.try_into_panic() {
                            panic::resume_unwind(reason);
                        }
                    }
                    self.connections
                        .retain(|connection| !connection.abort_handle.is_finished());
                }
                request = self.request_rx.recv() => {
                    let flow = if let Some(request) = request {
                        self.process_request(request).await
                    } else {
                        ControlFlow::Break(())
                    };
                    match flow {
                        ControlFlow::Continue(_) => continue,
                        ControlFlow::Break(_) => break,
                    }
                }
            }
        }

        self.connections.clear(); // drop all channels to cause the `run_sender` loops to stop
        self.connection_pool.join_all().await;
    }

    async fn process_request(&mut self, request: Request) -> ControlFlow<()> {
        match request {
            Request::Invoke { dc_id, body, tx } => {
                let connection = match self
                    .connections
                    .iter()
                    .find(|connection| connection.dc_id == dc_id)
                {
                    Some(connection) => connection,
                    None => match self.create_connection(dc_id).await {
                        Ok(x) => x,
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return ControlFlow::Continue(());
                        }
                    },
                };
                let _ = connection.rpc_tx.send(Rpc { body, tx });
                ControlFlow::Continue(())
            }
            Request::Disconnect { dc_id } => {
                self.connections.retain(|connection| {
                    if connection.dc_id == dc_id {
                        connection.abort_handle.abort();
                        false
                    } else {
                        true
                    }
                });
                ControlFlow::Continue(())
            }
            Request::Quit => ControlFlow::Break(()),
        }
    }

    async fn create_connection(&mut self, dc_id: i32) -> Result<&ConnectionInfo, InvocationError> {
        let mut dc_option = match self.session.dc_option(dc_id)? {
            Some(x) => x,
            None => return Err(InvocationError::InvalidDc),
        };

        let sender = self.connect_sender(&dc_option).await?;

        dc_option.auth_key = Some(sender.auth_key());
        self.session.set_dc_option(&dc_option).await?;

        let (rpc_tx, rpc_rx) = mpsc::unbounded_channel();
        let abort_handle = self.connection_pool.spawn(run_sender(
            sender,
            rpc_rx,
            self.updates_tx.clone(),
            dc_option.id == self.session.home_dc_id()?,
        ));
        self.connections.push(ConnectionInfo {
            dc_id,
            rpc_tx,
            abort_handle,
        });
        Ok(self.connections.last().unwrap())
    }

    async fn connect_sender(
        &mut self,
        dc_option: &DcOption,
    ) -> Result<Sender<transport::Full, mtp::Encrypted>, InvocationError> {
        let transport = transport::Full::new;

        let address = if self.connection_params.use_ipv6 {
            dc_option.ipv6.into()
        } else {
            dc_option.ipv4.into()
        };

        let attempts = connection_attempts(address, &self.connection_params);

        let init_connection = tl::functions::InvokeWithLayer {
            layer: tl::LAYER,
            query: tl::functions::InitConnection {
                api_id: self.api_id,
                device_model: self.connection_params.device_model.clone(),
                system_version: self.connection_params.system_version.clone(),
                app_version: self.connection_params.app_version.clone(),
                system_lang_code: self.connection_params.system_lang_code.clone(),
                lang_pack: "".into(),
                lang_code: self.connection_params.lang_code.clone(),
                proxy: None,
                params: None,
                query: tl::functions::help::GetConfig {},
            },
        };

        let mut sender = self.establish_stream(dc_option, transport, &attempts).await?;

        let enums::Config::Config(remote_config) = match sender.invoke(&init_connection).await {
            Ok(config) => config,
            Err(InvocationError::Transport(transport::Error::BadStatus { status: 404 })) => {
                sender = self.establish_stream(dc_option, transport, &attempts).await?;
                sender.invoke(&init_connection).await?
            }
            Err(e) => return Err(e),
        };

        self.update_config(remote_config).await?;

        Ok(sender)
    }

    /// Connect to a datacenter by trying each configured attempt in order.
    ///
    /// A proxied attempt is bounded by [`PROXY_CONNECT_TIMEOUT`]; when it fails
    /// with an I/O error or the timeout expires and a direct attempt follows
    /// (see [`ConnectionParams::proxy_fallback`]), the connection falls back to
    /// it. Direct attempts are never retried or replaced.
    async fn establish_stream(
        &self,
        dc_option: &DcOption,
        transport: impl Fn() -> Transport,
        attempts: &[ServerAddr],
    ) -> Result<Sender<transport::Full, mtp::Encrypted>, InvocationError> {
        for (index, addr) in attempts.iter().enumerate() {
            let attempt = async {
                if let Some(auth_key) = dc_option.auth_key {
                    connect_with_auth(transport(), addr.clone(), auth_key)
                        .await
                        .map_err(InvocationError::Io)
                } else {
                    connect(transport(), addr.clone()).await
                }
            };

            #[cfg(feature = "proxy")]
            let result = if matches!(addr, ServerAddr::Proxied { .. }) {
                match tokio::time::timeout(PROXY_CONNECT_TIMEOUT, attempt).await {
                    Ok(result) => result,
                    Err(_) => Err(InvocationError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "proxy connection timed out",
                    ))),
                }
            } else {
                attempt.await
            };

            #[cfg(not(feature = "proxy"))]
            let result = attempt.await;

            match result {
                Ok(sender) => return Ok(sender),
                Err(error) => {
                    #[cfg(feature = "proxy")]
                    let fallback = matches!(addr, ServerAddr::Proxied { .. })
                        && matches!(error, InvocationError::Io(_))
                        && index + 1 < attempts.len();
                    #[cfg(not(feature = "proxy"))]
                    let fallback = false;
                    if !fallback {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("every datacenter has at least one connection attempt")
    }

    async fn update_config(&mut self, config: tl::types::Config) -> Result<(), InvocationError> {
        for option in config
            .dc_options
            .iter()
            .map(|tl::enums::DcOption::Option(option)| option)
            .filter(|option| !option.media_only && !option.tcpo_only && option.r#static)
        {
            let mut dc_option = self
                .session
                .dc_option(option.id)?
                .unwrap_or_else(|| DcOption {
                    id: option.id,
                    ipv4: SocketAddrV4::new(Ipv4Addr::from_bits(0), 0),
                    ipv6: SocketAddrV6::new(Ipv6Addr::from_bits(0), 0, 0, 0),
                    auth_key: None,
                });
            if option.ipv6 {
                dc_option.ipv6 = SocketAddrV6::new(
                    option
                        .ip_address
                        .parse()
                        .expect("Telegram to return a valid IPv6 address"),
                    option.port as _,
                    0,
                    0,
                );
            } else {
                dc_option.ipv4 = SocketAddrV4::new(
                    option
                        .ip_address
                        .parse()
                        .expect("Telegram to return a valid IPv4 address"),
                    option.port as _,
                );
                if dc_option.ipv6.ip().to_bits() == 0 {
                    dc_option.ipv6 = SocketAddrV6::new(
                        dc_option.ipv4.ip().to_ipv6_mapped(),
                        dc_option.ipv4.port(),
                        0,
                        0,
                    )
                }
            }
        }
        Ok(())
    }
}

async fn run_sender(
    mut sender: Sender<Transport, grammers_mtproto::mtp::Encrypted>,
    mut rpc_rx: mpsc::UnboundedReceiver<Rpc>,
    updates: mpsc::UnboundedSender<UpdatesLike>,
    home_sender: bool,
) -> Result<(), ReadError> {
    loop {
        tokio::select! {
            step = sender.step() => match step {
                Ok(all_new_updates) => all_new_updates.into_iter().for_each(|new_updates| {
                    let _ = updates.send(new_updates);
                }),
                Err(err) => {
                    if home_sender {
                        let _ = updates.send(UpdatesLike::ConnectionClosed);
                    }
                    break Err(err)
                },
            },
            rpc = rpc_rx.recv() => match rpc {
                Some(rpc) => sender.enqueue_body(rpc.body, rpc.tx),
                None => break Ok(()),
            },
        }
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invoke { dc_id, body, tx } => f
                .debug_struct("Invoke")
                .field("dc_id", dc_id)
                .field(
                    "request",
                    &body[..4]
                        .try_into()
                        .map(|constructor_id| tl::name_for_id(u32::from_le_bytes(constructor_id)))
                        .unwrap_or("?"),
                )
                .field("tx", tx)
                .finish(),
            Self::Disconnect { dc_id } => {
                f.debug_struct("Disconnect").field("dc_id", dc_id).finish()
            }
            Self::Quit => write!(f, "Quit"),
        }
    }
}

struct Eraser<S: Session>(Arc<S>);

impl<S> Session for Eraser<S>
where
    S: Session,
    S::Error: std::error::Error + Send + Sync,
{
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        Arc::clone(&self.0).home_dc_id().map_err(|e| e.into())
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            Arc::clone(&self.0)
                .set_home_dc_id(dc_id)
                .await
                .map_err(|e| e.into())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        Arc::clone(&self.0).dc_option(dc_id).map_err(|e| e.into())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            Arc::clone(&self.0)
                .set_dc_option(&dc_option)
                .await
                .map_err(|e| e.into())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move { Arc::clone(&self.0).peer(peer).await.map_err(|e| e.into()) })
    }

    fn peer_ref(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerRef>, Self::Error>> {
        Box::pin(async move {
            Arc::clone(&self.0)
                .peer_ref(peer)
                .await
                .map_err(|e| e.into())
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        let peer = peer.clone();
        Box::pin(async move {
            Arc::clone(&self.0)
                .cache_peer(&peer)
                .await
                .map_err(|e| e.into())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async {
            Arc::clone(&self.0)
                .updates_state()
                .await
                .map_err(|e| e.into())
        })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async {
            Arc::clone(&self.0)
                .set_update_state(update)
                .await
                .map_err(|e| e.into())
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "proxy")]
    fn proxy_attempts_fall_back_only_when_enabled() {
        use super::*;
        let address = std::net::SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 443));
        let params = ConnectionParams::default();

        assert!(matches!(
            connection_attempts(address, &params).as_slice(),
            [ServerAddr::Tcp { .. }]
        ));

        let mut params = ConnectionParams::default();
        params.proxy_url = Some("socks5://127.0.0.1:9050".to_owned());
        assert!(matches!(
            connection_attempts(address, &params).as_slice(),
            [ServerAddr::Proxied { .. }]
        ));

        params.proxy_fallback = true;
        assert!(matches!(
            connection_attempts(address, &params).as_slice(),
            [ServerAddr::Proxied { .. }, ServerAddr::Tcp { .. }]
        ));
    }
}
