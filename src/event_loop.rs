use std::sync::atomic::Ordering::Relaxed;
use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{atomic::AtomicBool, Arc},
};

use crate::{
    handshake::Handshake,
    packet::Packet::{
        Ack, Data, Fin, FinOk, Init, InitOk, KeepAlive, KeepAliveOk, Nak, Syn, SynAck,
    },
    packet::{Packet, SeqNum},
    MessageKind, MTU, TIMEOUT,
};

const WINDOW_SIZE: usize = 128;

use tokio::{
    net::UdpSocket,
    sync::{mpsc, Notify},
};
use tokio_util::time::{delay_queue::Key, DelayQueue};

use crate::Message;

#[allow(unused_macros)]
macro_rules! debug {
    ($reader:ident, $sender:ident, $sig:ident) => {
        let reader = match &$reader {
            Some(_) => "Reader",
            None => "",
        };
        let sender = match &$sender {
            Some(_) => "Sender",
            None => "",
        };
        dbg!(reader, sender, &$sig);
    };
}

#[derive(Debug)]
enum Sig<'a> {
    Packet(Packet<'a>),
    Timeout(DelayEvent),
    SocketError(std::io::Error),
    StartSendingFile(Message),
    KeepAlive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelayEvent {
    Ack(SeqNum),
    Fin,
    KeepAliveExpired,
}

struct SendState {
    message: Message,
    timeout_keys: HashMap<SeqNum, Key>,
    fin_timeout_key: Option<Key>,
    next_to_send: SeqNum,
}

impl SendState {
    #[inline]
    fn seq_num_count(&mut self) -> usize {
        let payload_size = self.message.payload_size as usize;
        let transfer = self.message.payload.len();
        (transfer + payload_size - 1) / payload_size
    }
}

struct ReceiveState {
    recv_bytes: Vec<u8>,
    payload_size: usize,
    name: Option<String>,
}

// TODO: Use NAK's somehow
pub(crate) async fn event_loop(
    socket: UdpSocket,
    _handshake: Handshake,
    shutdown: Arc<AtomicBool>,
    api_sender_notify_tx: Arc<Notify>,
    mut api_sender_rx: mpsc::Receiver<Message>,
    api_received_messages_tx: mpsc::Sender<Message>,
) -> std::io::Result<UdpSocket> {
    let mut timers = DelayQueue::new();
    let mut buf = [0; MTU];
    let mut reader: Option<ReceiveState> = None;
    let mut sender: Option<SendState> = None;
    let mut keep_alive_key = None;

    'event_loop: loop {
        // dbg!("Event loop tick");
        let delay_event = futures::future::poll_fn(|cx| timers.poll_expired(cx));
        let sig = tokio::select! {
            biased;
            _ = async {}, if shutdown.load(Relaxed) => return Ok(socket),
            Some(expired) = delay_event => Sig::Timeout(expired.into_inner()),
            socket_res = socket.recv(&mut buf) => match socket_res {
                Ok(len) => match Packet::deserialize(&buf[..len]) {
                    Some(packet) => Sig::Packet(packet),
                    None => continue 'event_loop,
                }
                Err(e) => Sig::SocketError(e)
            },
            msg = api_sender_rx.recv() => {
                Sig::StartSendingFile(msg.expect("API sender channel closed"))
            }
            _ = tokio::time::sleep(TIMEOUT) => Sig::KeepAlive,
        };

        // debug!(reader, sender, sig);

        (reader, sender) = match (sig, reader, sender) {
            // The hottest path for reader
            (Sig::Packet(Data { seq_num, data }), Some(mut re), s) => {
                let payload_size = re.payload_size;
                // TODO: Nak maybe?
                debug_assert!(
                    data.len() <= payload_size,
                    "Data packet size is bigger then payload_size",
                );
                let bytes_before = seq_num.0 as usize * payload_size;
                re.recv_bytes[bytes_before..][..data.len()].copy_from_slice(data);
                let len = Packet::Ack(seq_num).serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (Some(re), s)
            }

            // The hottest path for sender
            (Sig::Packet(Ack(seq_num_ack)), r, Some(mut se)) => 'block: {
                timers.remove(&se.timeout_keys.remove(&seq_num_ack).unwrap());

                let seq_num = se.next_to_send;

                if seq_num == SeqNum(se.seq_num_count() as u32) {
                    // What if NAK ?
                    // Is race possible?
                    if se.timeout_keys.is_empty() {
                        let len = Packet::Fin.serialize(&mut buf);
                        socket.send(&buf[..len]).await?;
                        let key = timers.insert(DelayEvent::Fin, TIMEOUT);
                        se.fin_timeout_key = Some(key);
                    }
                    break 'block (r, Some(se));
                }

                se.next_to_send += SeqNum(1);

                send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                (r, Some(se))
            }

            // Receiver
            (Sig::Packet(Fin), Some(re), s) => {
                let len = Packet::FinOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;

                let message = Message {
                    payload: re.recv_bytes,
                    payload_size: re.payload_size as u16,
                    kind: match re.name {
                        Some(name) => MessageKind::File(name),
                        None => MessageKind::Text,
                    },
                };

                api_received_messages_tx
                    .send(message)
                    .await
                    .expect("TODO: convert to io error");

                (None, s)
            }
            #[rustfmt::skip]
            (Sig::Packet(Init { payload, transfer, name }), re, s) => 'b: {
                if let Some(r) = re {
                    if !(r.recv_bytes.len() == transfer as usize
                        && r.payload_size == payload as usize
                        && r.name.as_ref().zip(name).map_or(true, |(a, b)| a == b))
                    {
                        break 'b (Some(r), s);
                    }
                }

                let name = name.map(|s| s.to_string());

                let len = Packet::InitOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;

                let reader = Some(ReceiveState {
                    recv_bytes: vec![0; transfer as usize],
                    payload_size: payload as usize,
                    name,
                });

                (reader, s)
            }

            (Sig::SocketError(e), _, _) => match e.kind() {
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => return Ok(socket),
                _ => return Err(e),
            },

            // Sender
            (Sig::Timeout(DelayEvent::Fin), r, Some(mut se)) => {
                let len = Packet::Fin.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                let key = timers.insert(DelayEvent::Fin, TIMEOUT);
                se.fin_timeout_key = Some(key);
                (r, Some(se))
            }
            (Sig::Packet(FinOk), r, Some(mut se)) => {
                if se.fin_timeout_key.take().is_some() {
                    api_sender_notify_tx.notify_waiters();
                    (r, None)
                } else {
                    (r, Some(se))
                }
            }
            (Sig::StartSendingFile(message), r, None) => {
                let packet = Packet::Init {
                    payload: message.payload_size,
                    transfer: message.payload.len() as u32,
                    name: match &message.kind {
                        MessageKind::File(name) => Some(name),
                        MessageKind::Text => None,
                    },
                };
                let len = packet.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                let sender = Some(SendState {
                    message,
                    timeout_keys: HashMap::default(),
                    next_to_send: SeqNum(0),
                    fin_timeout_key: None,
                });
                (r, sender)
            }

            (Sig::Packet(InitOk), r, Some(mut se)) => {
                let send_count = WINDOW_SIZE.min(se.seq_num_count());
                for left in 0..send_count {
                    let seq_num = SeqNum(left as u32);
                    send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;
                }
                se.next_to_send = SeqNum(send_count as u32);

                (r, Some(se))
            }

            (
                Sig::Packet(Nak(seq_num)) | Sig::Timeout(DelayEvent::Ack(seq_num)),
                r,
                Some(mut se),
            ) => {
                send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                (r, Some(se))
            }

            // Shared
            (Sig::Packet(Syn), r, s) => (r, s),
            (Sig::Packet(SynAck(seq_num)), r, s) => {
                let len = Packet::Ack(seq_num).serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (r, s)
            }
            (Sig::StartSendingFile(_), _, Some(_)) => panic!("Should not send file in parallel."),
            (Sig::Packet(KeepAlive), r, s) => {
                let len = Packet::KeepAliveOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (r, s)
            }
            (
                Sig::Timeout(DelayEvent::Fin) | Sig::Packet(InitOk | FinOk | Ack(_) | Nak(_)),
                r,
                None,
            ) => (r, None),
            (Sig::Timeout(DelayEvent::Ack(_)), _, None) => {
                panic!("Delay events should be cleared properly.")
            }
            (Sig::Packet(Data { .. } | Fin), None, s) => (None, s),
            (Sig::Timeout(DelayEvent::KeepAliveExpired), _r, _s) => {
                return Ok(socket);
            }

            (Sig::KeepAlive, r, s) => {
                let len = Packet::KeepAlive.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                let key = timers.insert(DelayEvent::KeepAliveExpired, TIMEOUT);
                keep_alive_key = Some(key);
                (r, s)
            }
            (Sig::Packet(KeepAliveOk), r, s) => {
                if let Some(key) = keep_alive_key {
                    timers.remove(&key);
                }
                (r, s)
            }
        }
    }
}

#[inline]
async fn send_data_packet(
    sender: &mut SendState,
    socket: &UdpSocket,
    buf: &mut [u8],
    seq_num: SeqNum,
    timers: &mut DelayQueue<DelayEvent>,
) -> std::io::Result<()> {
    let msg = &sender.message;
    let payload_size = msg.payload_size as usize;
    let bytes_before = seq_num.0 as usize * payload_size;

    debug_assert!(bytes_before < msg.payload.len(), "Too many bytes sent.");

    let next_bytes = &msg.payload[bytes_before..];

    let len = Packet::Data {
        seq_num,
        data: &next_bytes[..payload_size.min(next_bytes.len())],
    }
    .serialize(buf);
    socket.send(&buf[..len]).await?;

    let key = timers.insert(DelayEvent::Ack(seq_num), TIMEOUT);

    // DelayQueue could yield the same key
    if let Some(removed_key) = sender.timeout_keys.insert(seq_num, key) {
        if key != removed_key {
            timers.remove(&removed_key);
        }
    }

    std::io::Result::Ok(())
}
