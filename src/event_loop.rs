use std::{net::SocketAddr, sync::Arc};

use crate::{
    handshake::Handshake,
    packet::{Packet, SeqNum},
    MTU,
};

// const WINDOW_SIZE: usize = 128;

use tokio::{
    net::UdpSocket,
    sync::{mpsc, Notify},
};
use tokio_util::time::DelayQueue;

use crate::Message;

#[derive(Debug)]
enum Sig {
    Packet(Packet, SocketAddr),
    Timeout(DelayEvent),
    SocketError(std::io::Error),
    StartSendingFile(Message),
}

#[derive(Debug)]
enum DelayEvent {
    AckTimeout(SeqNum),
}

struct SendState {
    send_message: Message,
}

struct ReceiveState {
    recv_bytes: Vec<u8>
}

//  TODO: keep sending Ack to SeqAcks
pub(crate) async fn event_loop(
    socket: UdpSocket,
    _handshake: Handshake,
    shutdown: Arc<Notify>,
    mut _api_sender_notify_tx: Arc<Notify>,
    mut api_sender_rx: mpsc::Receiver<Message>,
    mut _api_received_messages_tx: mpsc::Sender<Message>,
) -> std::io::Result<UdpSocket> {
    let mut timers = DelayQueue::<DelayEvent>::new();
    let mut buf = [0; MTU];

    'event_loop: loop {
        let delay_event = futures::future::poll_fn(|cx| timers.poll_expired(cx));
        let sig = async {
            tokio::select! {
                // what if none?
                Some(expired) = delay_event => Some(Sig::Timeout(expired.into_inner())),
                socket_res = socket.recv_from(&mut buf) => Some(match socket_res {
                    Ok((len, addr)) => Sig::Packet(Packet::deserialize(&buf[..len])?, addr),
                    Err(e) => Sig::SocketError(e)
                }),
                msg = api_sender_rx.recv() => {
                    Some(Sig::StartSendingFile(msg.expect("API sender channel closed")))
                }
            }
        };

        let sig = tokio::select! {
            biased;
            _ = shutdown.notified() => return Ok(socket),
            sig = sig => match sig {
                Some(sig) => sig,
                None => continue 'event_loop
            }
        };

        match sig {
            _ => todo!(),
        }
    }
}

/*

for _ in 0..WINDOW_SIZE {
    todo!("Send packets");
}

*/
