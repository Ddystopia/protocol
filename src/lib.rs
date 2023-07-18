#![allow(clippy::missing_errors_doc)]

pub mod packet;
mod event_loop;

use std::io;

use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    sync::mpsc,
    task::JoinHandle,
};

pub enum Message {
    TextMessage,
    File,
}

pub struct Server {
    socket: UdpSocket,
}

pub struct Connection {
    socket: Option<UdpSocket>,
    event_loop: JoinHandle<()>,

    api_sender_tx: mpsc::Sender<Message>,
    api_sender_notify_rx: mpsc::Receiver<()>,
    api_received_messages_rx: mpsc::Receiver<Message>,
}

pub struct Receiver<'a>(&'a mut mpsc::Receiver<Message>);

//  TODO: Replace mpsc with channel of capacity 1 that not dies as oneshot
//        Maybe write my own, "Atomics and Locks" is a great book
//        `AtomicPtr`?
pub struct Sender<'a> {
    sender: &'a mut mpsc::Sender<Message>,
    signal: &'a mut mpsc::Receiver<()>,
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

    pub async fn connect(self, addr: impl ToSocketAddrs) -> io::Result<Connection> {
        self.socket.connect(addr).await?;

        let (api_received_messages_tx, api_received_messages_rx) = mpsc::channel(3);
        let (api_sender_tx, api_sender_rx) = mpsc::channel(1);
        let (api_sender_notify_tx, api_sender_notify_rx) = mpsc::channel(1);

        let event_loop = tokio::spawn(event_loop::event_loop(
            api_sender_notify_tx,
            api_sender_rx,
            api_received_messages_tx,
        ));

        Ok(Connection {
            socket: Some(self.socket),
            event_loop,
            api_sender_tx,
            api_sender_notify_rx,
            api_received_messages_rx,
        })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.event_loop.abort();
    }
}

impl Sender<'_> {
    pub async fn send(self, message: Message) -> Result<(), ()> {
        self.sender.send(message).await.map_err(|_| ())?;
        self.signal.recv().await.ok_or(())
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
        let signal = &mut self.api_sender_notify_rx;

        let sender = Sender { sender, signal };
        let receiver = Receiver(&mut self.api_received_messages_rx);

        (sender, receiver)
    }


    #[allow(clippy::missing_panics_doc)]
    pub fn disconnect(mut self) -> Server {
        Server {
            socket: self.socket.take().expect("Socket is always present")
        }
    }
}

