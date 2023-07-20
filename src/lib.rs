#![allow(clippy::missing_errors_doc)]

mod event_loop;
pub mod handshake;
pub mod packet;

use std::{io, sync::Arc};

use handshake::Handshake;
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    sync::{
        mpsc::{self, error::SendError},
        Notify,
    },
    task::JoinHandle,
};

pub(crate) const MTU: usize = 1500;

#[derive(Debug)]
pub enum Message {
    TextMessage,
    File,
}

pub struct Server {
    socket: UdpSocket,
}

pub struct Connection {
    event_loop: Option<JoinHandle<io::Result<UdpSocket>>>,
    api_received_messages_rx: mpsc::Receiver<Message>,
    api_sender_tx: mpsc::Sender<Message>,
    api_sender_notify_rx: Arc<Notify>,
    shutdown_tx: Arc<Notify>,
}

pub struct Receiver<'a>(&'a mut mpsc::Receiver<Message>);

//  TODO: Replace mpsc with channel of capacity 1 that not dies as oneshot
//        Maybe write my own, "Atomics and Locks" is a great book
//        `AtomicPtr`?
pub struct Sender<'a> {
    sender: &'a mut mpsc::Sender<Message>,
    signal: &'a Notify,
}

impl Server {
    /// This function will create a new protocol instance and attempt to bind it
    /// to the `addr` provided.
    ///
    /// Binding with a port number of 0 will request that the OS assigns a port
    /// to this listener. The port allocated can be queried via the `local_addr`
    /// method.
    pub async fn bind(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self { socket })
    }

    pub async fn connect(self, addr: impl ToSocketAddrs + Clone) -> io::Result<Connection> {
        let handshake = handshake::handshake_active(&self.socket, addr).await?;
        Ok(self.create_connection(handshake))
    }

    pub async fn listen(self) -> io::Result<Connection> {
        let handshake = handshake::handshake_passive(&self.socket).await?;
        Ok(self.create_connection(handshake))
    }

    fn create_connection(self, handshake: Handshake) -> Connection {
        let (api_received_messages_tx, api_received_messages_rx) = mpsc::channel(3);
        let (api_sender_tx, api_sender_rx) = mpsc::channel(1);
        let api_sender_notify_rx = Arc::new(Notify::new());
        let shutdown_tx = Arc::new(Notify::new());

        let event_loop = Some(tokio::spawn(event_loop::event_loop(
            self.socket,
            handshake,
            shutdown_tx.clone(),
            api_sender_notify_rx.clone(),
            api_sender_rx,
            api_received_messages_tx,
        )));

        Connection {
            event_loop,
            api_received_messages_rx,
            api_sender_tx,
            api_sender_notify_rx,
            shutdown_tx,
        }
    }
}

impl Sender<'_> {
    pub async fn send(&mut self, message: Message) -> Result<(), SendError<Message>> {
        self.sender.send(message).await?;
        self.signal.notified().await;
        Ok(())
    }
}

impl Receiver<'_> {
    pub async fn recv(&mut self) -> Option<Message> {
        self.0.recv().await
    }
}

impl Connection {
    pub fn split(&mut self) -> (Sender, Receiver) {
        let sender = &mut self.api_sender_tx;
        let signal = &self.api_sender_notify_rx;

        let sender = Sender { sender, signal };
        let receiver = Receiver(&mut self.api_received_messages_rx);

        (sender, receiver)
    }

    #[allow(clippy::missing_panics_doc)]
    pub async fn disconnect(mut self) -> Result<Server, ()> {
        self.shutdown_tx.notify_waiters();
        let socket = tokio::join!(self.event_loop.take().unwrap())
            .0
            .map_err(|_| ())?
            .map_err(|_| ())?;
        Ok(Server { socket })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(event_loop) = self.event_loop.take() {
            self.shutdown_tx.notify_waiters();
            event_loop.abort();
        }
    }
}
