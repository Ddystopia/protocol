#![allow(clippy::missing_errors_doc)]

// TODO: PGO

mod event_loop;
mod handshake;
mod packet;

use std::{io, sync::{Arc, atomic::AtomicBool}, time::Duration};
use std::sync::atomic::Ordering::Relaxed;

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

pub(crate) const TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MessageKind {
    Text,
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    kind: MessageKind,
    payload: Vec<u8>,
    payload_size: u16,
}

pub struct Server {
    socket: UdpSocket,
}

pub struct Connection {
    event_loop: Option<JoinHandle<io::Result<UdpSocket>>>,
    api_received_messages_rx: mpsc::Receiver<Message>,
    api_sender_tx: mpsc::Sender<Message>,
    api_sender_notify_rx: Arc<Notify>,
    shutdown_tx: Arc<AtomicBool>,
}

pub struct Receiver<'a>(&'a mut mpsc::Receiver<Message>);

//  TODO: Replace mpsc with channel of capacity 1 that not dies as oneshot
//        Maybe write my own, "Atomics and Locks" is a great book
//        `AtomicPtr`?
//  ^^^^^ I think it is not worth it, that channel still has capacity 1
//        And does not grow.
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

    pub async fn connect(self, addr: impl ToSocketAddrs) -> io::Result<Connection> {
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
        let shutdown_tx = Arc::new(AtomicBool::new(false));
        let api_sender_notify_rx = Arc::new(Notify::new());

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
    // TODO: Custom error type, that one leaks internal representation
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

impl Message {
    #[must_use]
    pub fn file(payload: Vec<u8>, payload_size: u16, filename: String) -> Self {
        Self {
            kind: MessageKind::File(filename),
            payload,
            payload_size,
        }
    }
    #[must_use]
    pub fn text(payload: Vec<u8>, payload_size: u16) -> Self {
        Self {
            kind: MessageKind::Text,
            payload,
            payload_size,
        }
    }
}

impl Connection {
    #[must_use]
    pub fn split(&mut self) -> (Sender, Receiver) {
        let sender = &mut self.api_sender_tx;
        let signal = &self.api_sender_notify_rx;

        let sender = Sender { sender, signal };
        let receiver = Receiver(&mut self.api_received_messages_rx);

        (sender, receiver)
    }

    #[allow(clippy::missing_panics_doc)]
    pub async fn disconnect(mut self) -> Result<Server, ()> {
        self.shutdown_tx.store(true, Relaxed);
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
            self.shutdown_tx.store(true, Relaxed);
            event_loop.abort();
        }
    }
}

#[cfg(test)]
mod test {
    use tokio::time::Instant;

    use super::*;

    #[tokio::test]
    #[allow(clippy::diverging_sub_expression)]
    #[allow(unreachable_code)]
    async fn hello_world() {
        const ADDR1: &str = "127.0.0.1:10201";
        const ADDR2: &str = "127.0.0.1:10202";

        let size = 400 * 2usize.pow(20);
        let msg = Message::file(vec![5u8; size], 1496, "Ivakura.txt".to_string());
        let msg_clone = msg.clone();
        dbg!("Here");
        let start = Instant::now();

        let h1 = tokio::spawn(async move {
            let server = Server::bind(ADDR1).await.unwrap();
            let mut connection = server.connect(ADDR2).await.unwrap();
            let (mut tx, _) = connection.split();
            tx.send(msg_clone).await
        });
        let h2 = tokio::spawn(async move {
            let server = Server::bind(ADDR2).await.unwrap();
            let mut connection = server.listen().await.unwrap();
            let (_, mut rx) = connection.split();
            rx.recv().await
        });

        let (r1, r2) = tokio::join!(h1, h2);
        let end = Instant::now();
        r1.unwrap().unwrap();
        let transfered_message = r2.unwrap().unwrap();
        assert_eq!(transfered_message, msg);
        println!(
            "Speed: {}Mib/sec",
            (size / 1000 * 8) / (end - start).as_millis() as usize
        );
    }
}
