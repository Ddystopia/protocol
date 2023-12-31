#![allow(clippy::missing_errors_doc)]
// TODO: rename `FIN` to something else
//       it is not closing connection, it ends the transfer
//       maybe `END` or `EOF`?

mod event_loop;
mod handshake;
#[cfg(feature = "mock")]
pub mod mock;
mod packet;

use std::{fmt::Debug, io, num::TryFromIntError, sync::Arc, time::Duration};

use tokio::{
    sync::{
        mpsc::{self, error::SendError},
        oneshot, Notify,
    },
    task::JoinHandle,
};

use handshake::Handshake;

const MTU: usize = 1500;
pub(crate) const MSS: usize = MTU - 8 /* UDP */ - 20 /* IP */;
pub const MAX_TRANSFER_SIZE: u16 = MSS as u16 - 4 /* Header for `Data` */;

pub(crate) const ACK_TIMEOUT: Duration = Duration::from_millis(10);
pub(crate) const KA_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(feature = "mock")]
use std::sync::atomic::AtomicU32;
#[cfg(feature = "mock")]
pub(crate) static NUM_LOSS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "mock")]
pub(crate) static DEN_LOSS: AtomicU32 = AtomicU32::new(1);

#[cfg(feature = "mock")]
pub(crate) type UdpSocket = mock::Mock;
#[cfg(feature = "mock")]
pub(crate) use mock::ToSocketAddrs;

#[cfg(not(feature = "mock"))]
pub(crate) type UdpSocket = tokio::net::UdpSocket;
#[cfg(not(feature = "mock"))]
pub(crate) use tokio::net::ToSocketAddrs;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    data: MessageData,
    payload_size: u16,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MessageData {
    Text(String),
    File { name: String, payload: Vec<u8> },
}

impl Debug for MessageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(s) => s.fmt(f),
            Self::File { name, payload } => f
                .debug_struct("MessageData::File")
                .field("name", name)
                .field("payload (first 20)", &&payload[..payload.len().min(20)])
                .finish(),
        }
    }
}

pub struct Server {
    socket: UdpSocket,
}

pub struct Connection {
    event_loop: Option<(
        JoinHandle<io::Result<UdpSocket>>,
        oneshot::Sender<ShutdownSignalZST>,
    )>,
    api_received_messages_rx: mpsc::Receiver<Message>,
    api_sender_tx: mpsc::Sender<Message>,
    api_sender_notify_rx: Arc<Notify>,
}

#[derive(Debug)]
pub(crate) struct ShutdownSignalZST;

pub struct Receiver<'a>(&'a mut mpsc::Receiver<Message>);

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

    pub async fn connect(self, addr: impl ToSocketAddrs + Copy) -> io::Result<Connection> {
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
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let api_sender_notify_rx = Arc::new(Notify::new());

        let event_loop_handle = tokio::spawn(event_loop::event_loop(
            self.socket,
            handshake,
            shutdown_rx,
            api_sender_notify_rx.clone(),
            api_sender_rx,
            api_received_messages_tx,
        ));

        Connection {
            event_loop: Some((event_loop_handle, shutdown_tx)),
            api_received_messages_rx,
            api_sender_tx,
            api_sender_notify_rx,
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
    /// Creates a new message with the provided payload.
    /// # Panics
    /// - If the total transfer size is bigger then 4GiB.
    /// - If the payload size is bigger then `MAX_PAYLOAD_SIZE` bytes.
    /// - If the filename is bigger then 22 bytes.
    #[must_use]
    pub fn file(payload: Vec<u8>, payload_size: u16, filename: String) -> Self {
        assert!(u32::try_from(payload.len()).is_ok(), "Payload is too big");
        assert!(payload_size <= MAX_TRANSFER_SIZE, "Payload is too big");
        assert!(filename.as_bytes().len() <= 22, "Filename is too big");
        Self {
            data: MessageData::File {
                name: filename,
                payload,
            },
            payload_size,
        }
    }
    pub fn text(payload: String, payload_size: u16) -> Result<Self, TryFromIntError> {
        Ok(Self {
            payload_size,
            data: MessageData::Text(payload),
        })
    }

    #[must_use]
    pub fn get_data(&self) -> &MessageData {
        &self.data
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
        let (event_loop_handle, shutdown_tx) = self.event_loop.take().unwrap();
        shutdown_tx
            .send(ShutdownSignalZST)
            .expect("Error while sending shutdown signal");
        let socket = tokio::join!(event_loop_handle).0.map_err(|_| ())?.unwrap();
        Ok(Server { socket })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some((event_loop_handle, shutdown_tx)) = self.event_loop.take() {
            shutdown_tx
                .send(ShutdownSignalZST)
                .expect("Error while sending shutdown signal");
            event_loop_handle.abort();
        }
    }
}

#[cfg(test)]
pub(crate) mod test {
    #![allow(clippy::type_complexity)]
    use super::*;
    use tokio::time::{self, Instant};

    #[tokio::test]
    async fn hello_world() {
        const ADDR1: &str = "127.0.0.1:10205";
        const ADDR2: &str = "127.0.0.1:10206";

        let message = "Hello, world!";
        let msg = Message::text(message.to_string(), message.len().try_into().unwrap()).unwrap();

        let h1 = tokio::spawn(async move {
            let server = Server::bind(ADDR1).await.unwrap();
            let mut connection = server.connect(ADDR2).await.unwrap();
            let (mut tx, _) = connection.split();
            tx.send(msg).await
        });
        let h2 = tokio::spawn(async move {
            let server = Server::bind(ADDR2).await.unwrap();
            let mut connection = server.listen().await.unwrap();
            let (_, mut rx) = connection.split();
            rx.recv().await
        });

        let (r1, r2) = tokio::join!(h1, h2);
        r1.unwrap().unwrap();
        let transfered_message = r2.unwrap().unwrap();
        match &transfered_message.data {
            MessageData::File { .. } => {
                assert!(matches!(transfered_message.data, MessageData::Text(_)));
            }
            MessageData::Text(text) => {
                println!("Message: {message}");
                assert!(text == message);
            }
        }
    }

    #[tokio::test]
    async fn random_data() {
        const ADDR1: &str = "127.0.0.1:10207";
        const ADDR2: &str = "127.0.0.1:10208";

        let size = 40 * 2usize.pow(20);
        let mut data = vec![0; size];
        for (i, x) in data.iter_mut().enumerate() {
            *x = (i % 256) as u8;
        }
        let name = "1234567890123456789012";
        let msg = Message::file(data, MAX_TRANSFER_SIZE, name.into());
        let msg_clone = msg.clone();
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
        let duration = start.elapsed();
        r1.unwrap().unwrap();
        let transfered_message = r2.unwrap().unwrap();
        assert_eq!(transfered_message, msg);
        println!(
            "Speed: {} Mib/sec",
            (size / 1000 * 8)
                .checked_div(duration.as_millis().try_into().unwrap())
                .unwrap_or(0)
        );
    }

    #[tokio::test]
    async fn keep_alive() {
        const ADDR1: &str = "127.0.0.1:10209";
        const ADDR2: &str = "127.0.0.1:10210";
        const DUR: Duration = Duration::from_secs(10);

        let h1 = tokio::spawn(async move {
            let server = Server::bind(ADDR1).await.unwrap();
            let connection = server.connect(ADDR2).await.unwrap();
            println!("Connected");
            time::sleep(DUR).await;

            println!("Disconnecting");
            let r = connection.disconnect().await;
            println!("Disconnected");
            r
        });
        let h2 = tokio::spawn(async move {
            let server = Server::bind(ADDR2).await.unwrap();
            let _connection = server.listen().await.unwrap();
            time::sleep(DUR).await;
        });

        let (r1, r2) = tokio::join!(h1, h2);
        r1.unwrap().expect("Disconnection Failed");
        r2.unwrap();
    }

    #[tokio::test]
    async fn two_messages() {
        const ADDR1: &str = "127.0.0.1:10213";
        const ADDR2: &str = "127.0.0.1:10214";
        const DUR: Duration = Duration::from_millis(2200);

        let message1: Vec<u8> = "Hello, world! 1 --".into();
        let message2 = "Hello, world! 2".into();
        let name = "HIII 💌 嗎 ".to_string();
        let payload_size = message1.len().try_into().unwrap();
        let msg1 = Message::file(message1, payload_size, name.clone());
        let msg2 = Message::file(message2, payload_size, name);

        let msg1_c = msg1.clone();
        let msg2_c = msg2.clone();

        let h1 = tokio::spawn(async move {
            let server = Server::bind(ADDR1).await.unwrap();
            let mut connection = server.connect(ADDR2).await.unwrap();
            let (mut tx, _) = connection.split();
            tx.send(msg1).await.expect("Failed to send 1");
            time::sleep(DUR).await;
            tx.send(msg2).await.expect("Failed to send 2");
        });
        let h2 = tokio::spawn(async move {
            let server = Server::bind(ADDR2).await.unwrap();
            let mut connection = server.listen().await.unwrap();
            let (_, mut rx) = connection.split();
            let msg1 = rx.recv().await.expect("Failed To Recv 1");
            let msg2 = rx.recv().await.expect("Failed To Recv 1");
            (msg1, msg2)
        });

        let (r1, r2) = tokio::join!(h1, h2);
        r1.unwrap();
        let (tm1, tm2) = r2.unwrap();
        assert!(msg1_c == tm1);
        assert!(msg2_c == tm2);
    }

    #[tokio::test]
    async fn unicode_name() {
        const ADDR1: &str = "127.0.0.1:10215";
        const ADDR2: &str = "127.0.0.1:10216";

        let name = "Hello, world! 💌";
        let message = "Hello, world!";
        let payload_size = message.len().try_into().unwrap();
        let msg = Message::file(name.to_string().into(), payload_size, message.to_string());
        let msg_c = msg.clone();

        let h1 = tokio::spawn(async move {
            let server = Server::bind(ADDR1).await.unwrap();
            let mut connection = server.connect(ADDR2).await.unwrap();
            let (mut tx, _) = connection.split();
            tx.send(msg).await.expect("Failed to send");
        });
        let h2 = tokio::spawn(async move {
            let server = Server::bind(ADDR2).await.unwrap();
            let mut connection = server.listen().await.unwrap();
            let (_, mut rx) = connection.split();
            rx.recv().await.expect("Failed To Recv")
        });

        let (r1, r2) = tokio::join!(h1, h2);
        r1.unwrap();
        let transfered_message = r2.unwrap();
        assert!(transfered_message == msg_c);
    }
}
