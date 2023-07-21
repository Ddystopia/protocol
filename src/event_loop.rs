use std::{
    collections::{HashMap, VecDeque},
    io::ErrorKind,
    net::SocketAddr,
    sync::Arc,
};

use crate::{
    handshake::Handshake,
    packet::Packet::{Ack, Data, Init, InitOk, KeepAlive, KeepAliveOk, Nak, Syn, SynAck},
    packet::{Packet, SeqNum},
    MessageKind, MTU, TIMEOUT,
};

const WINDOW_SIZE: usize = 128;

use tokio::{
    net::UdpSocket,
    sync::{mpsc, Notify},
    time::Instant,
};
use tokio_util::time::{delay_queue::Key, DelayQueue};

use crate::Message;

#[derive(Debug)]
enum Sig<'a> {
    Packet(Packet<'a>, SocketAddr),
    Timeout(DelayEvent),
    SocketError(std::io::Error),
    StartSendingFile(Message),
    SendPacket(SeqNum),
}

#[derive(Debug)]
enum DelayEvent {
    AckTimeout(SeqNum),
}

struct SendState {
    message: Message,
    queue: VecDeque<SeqNum>,
    timeout_keys: HashMap<SeqNum, Key>,
}

struct ReceiveState {
    recv_bytes: Vec<u8>,
    payload: usize,
    name: String,
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
    let mut reader: Option<ReceiveState> = None;
    let mut sender: Option<SendState> = None;
    let sender_queue_notify = Notify::new();

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
                _ = sender_queue_notify.notified() => {
                    Some(Sig::SendPacket(sender.as_mut().unwrap().queue.pop_front().unwrap()))
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

        (reader, sender) = match (sig, reader, sender) {
            (Sig::Packet(Syn, _), r, s) => (r, s),
            (Sig::Packet(SynAck(seq_num), _), r, s) => {
                let len = Packet::Ack(seq_num).serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (r, s)
            }
            (Sig::StartSendingFile(_), _, Some(_)) => panic!("Should not sent file in parallel."),
            (Sig::Packet(Packet::KeepAlive, _), r, s) => {
                let len = Packet::KeepAliveOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (r, s)
            }
            (Sig::Packet(Packet::KeepAliveOk, _), r, s) => todo!(),
            (Sig::Packet(Ack(_) | Nak(_), _), r, None) => (r, None),
            (Sig::SendPacket(_), _, None) => panic!("Should not send packet without sender state."),

            // Receiver
            #[rustfmt::skip]
            (Sig::Packet(Init { payload, transfer, name }, _), re, s) => 'b: {
                if let Some(r) = re {
                    if !(r.recv_bytes.len() == transfer as usize
                        && r.payload == payload as usize
                        && r.name == name)
                    {
                        break 'b (Some(r), s);
                    }
                }
                let reader = Some(ReceiveState {
                    recv_bytes: Vec::with_capacity(transfer as usize),
                    payload: payload as usize,
                    name: name.to_string(),
                });
                let len = Packet::InitOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (reader, s)
            }
            (Sig::Packet(Data { seq_num, data }, _), re, s) => {
                todo!()
            }
            (Sig::SocketError(e), _, _) => match e.kind() {
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => return Ok(socket),
                _ => return Err(e),
            },
            (Sig::Timeout(_), re, None) => todo!(),

            // Sender
            (Sig::StartSendingFile(message), r, None) => {
                let packet = Packet::Init {
                    payload: message.payload_size,
                    transfer: message.payload.len() as u32,
                    name: match &message.kind {
                        MessageKind::File(name) => &name,
                        MessageKind::Text => "",
                    },
                };
                let sender = Some(SendState {
                    message,
                    queue: (0..WINDOW_SIZE).map(|x| SeqNum(x as u32)).collect(),
                    timeout_keys: HashMap::default(),
                });
                (r, sender)
            }

            (Sig::Packet(InitOk, _), _, None) => todo!(),
            (Sig::Packet(InitOk, _), r, Some(se)) => todo!(),

            #[allow(clippy::unnested_or_patterns)]
            (Sig::Timeout(DelayEvent::AckTimeout(seq_num)), r, Some(mut se))
            | (Sig::Packet(Packet::Nak(seq_num), _), r, Some(mut se))
            | (Sig::SendPacket(seq_num), r, Some(mut se)) => {
                let left = seq_num.0 as usize;
                let msg = &se.message;
                let payload_size = msg.payload_size as usize;
                let len = Packet::Data {
                    seq_num,
                    data: &msg.payload[left * payload_size..][..payload_size],
                }
                .serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                let key =
                    timers.insert_at(DelayEvent::AckTimeout(seq_num), Instant::now() + TIMEOUT);
                se.timeout_keys
                    .insert(seq_num, key)
                    .map(|key| timers.remove(&key));
                (r, Some(se))
            }
            (Sig::Packet(Ack(seq_num), _), r, Some(mut se)) => {
                if let Some(key) = se.timeout_keys.remove(&seq_num) {
                    timers.remove(&key);
                }
                (r, Some(se))
            }
        }
    }
}
