use std::{
    io::ErrorKind,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc,
    },
};

use rustc_hash::FxHashMap;

use tokio::{
    net::UdpSocket,
    sync::{mpsc, Notify},
};
use tokio_util::time::{delay_queue::Key, DelayQueue};

use crate::{
    handshake::Handshake,
    packet::{
        ConnPacket::{self, KeepAlive, KeepAliveOk, Syn, SynAck, SynAckAck},
        PacketType,
        RecvPacket::{self, DataAck, FinOk, InitOk, Nak},
        SendPacket::{self, Data, Fin, Init},
    },
    packet::{Packet, SeqNum},
    Message, MessageKind, MTU, TIMEOUT,
};

const WINDOW_SIZE: usize = 128;

#[allow(unused_macros)]
macro_rules! debug {
    ($reader:ident, $sender:ident, $sig:ident) => {
        let side = match (&$reader, &$sender) {
            (Some(_), None) => "Reader",
            (None, Some(_)) => "Sender",
            (Some(_), Some(_)) => "Reader & Sender"
            (None, None) => "Empty"
        };
        dbg!(side, &$sig);
    };
}

#[derive(Debug)]
enum Sig<'a> {
    Conn(ConnSig),
    Recv(RecvSig<'a>),
    Send(SendSig),
}

#[derive(Debug)]
enum ConnSig {
    Packet(ConnPacket),
    SocketError(std::io::Error),
    KeepAlive,
    KeepAliveExpired,
}

#[derive(Debug)]
enum RecvSig<'a> {
    Packet(SendPacket<'a>),
}

#[derive(Debug)]
enum SendSig {
    Packet(RecvPacket),
    FinExpired,
    AckExpired(SeqNum),
    StartSendingFile(Message),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelayEvent {
    Ack(SeqNum),
    Fin,
    KeepAliveExpired,
}

struct SendState {
    message: Message,
    timeout_keys: FxHashMap<SeqNum, Key>,
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
    let mut buf = [0; MTU];
    let mut reader: Option<ReceiveState> = None;
    let mut sender: Option<SendState> = None;
    let mut timers = DelayQueue::new();
    let mut keep_alive_key = None;

    'event_loop: loop {
        let delay_event = futures::future::poll_fn(|cx| timers.poll_expired(cx));
        let sig = tokio::select! {
            biased;

            _ = async { /* would not react immediately */},
                if shutdown.load(Relaxed) => return Ok(socket),

            Some(expired) = delay_event => match expired.into_inner() {
                DelayEvent::Ack(s) => Sig::Send(SendSig::AckExpired(s)),
                DelayEvent::Fin => Sig::Send(SendSig::FinExpired),
                DelayEvent::KeepAliveExpired => Sig::Conn(ConnSig::KeepAliveExpired),
            },

            socket_res = socket.recv(&mut buf) => match socket_res {
                Ok(len) => match Packet::deserialize(&buf[..len]) {
                    Some(Packet::Conn(p)) => Sig::Conn(ConnSig::Packet(p)),
                    Some(Packet::Send(p)) => Sig::Recv(RecvSig::Packet(p)),
                    Some(Packet::Recv(p)) => Sig::Send(SendSig::Packet(p)),
                    None => continue 'event_loop,
                }
                Err(e) => Sig::Conn(ConnSig::SocketError(e))
            },

            msg = api_sender_rx.recv() => {
                Sig::Send(SendSig::StartSendingFile(msg.expect("API sender channel closed")))
            }

            _ = tokio::time::sleep(TIMEOUT) => Sig::Conn(ConnSig::KeepAlive)
        };

        match sig {
            Sig::Conn(sig) => match sig {
                ConnSig::Packet(Syn | SynAckAck(_)) => (),

                ConnSig::Packet(SynAck(seq_num)) => {
                    let len = RecvPacket::DataAck(seq_num).serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                }

                ConnSig::Packet(KeepAlive) => {
                    let len = ConnPacket::KeepAliveOk.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                }

                ConnSig::KeepAliveExpired => {
                    return Ok(socket);
                }

                ConnSig::KeepAlive => {
                    let len = ConnPacket::KeepAlive.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    let key = timers.insert(DelayEvent::KeepAliveExpired, TIMEOUT);
                    keep_alive_key = Some(key);
                }

                ConnSig::Packet(KeepAliveOk) => {
                    if let Some(key) = keep_alive_key {
                        timers.remove(&key);
                    }
                }

                ConnSig::SocketError(e) => match e.kind() {
                    ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => return Ok(socket),
                    ErrorKind::ConnectionRefused => todo!("Idk why but it happens"),
                    _ => return Err(e),
                },
            },

            Sig::Send(sig) => match (sig, sender) {
                (SendSig::Packet(DataAck(seq_num_ack)), Some(mut se)) => 'block: {
                    timers.remove(&se.timeout_keys.remove(&seq_num_ack).unwrap());

                    let seq_num = se.next_to_send;

                    // idk but with switched if it is faster. Maybe just a noise
                    if seq_num != SeqNum(se.seq_num_count() as u32) {
                        se.next_to_send += SeqNum(1);

                        send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                        sender = Some(se);
                        break 'block;
                    }

                    // What if NAK ?
                    // Is race possible?
                    if se.timeout_keys.is_empty() {
                        let len = Fin.serialize(&mut buf);
                        socket.send(&buf[..len]).await?;
                        let key = timers.insert(DelayEvent::Fin, TIMEOUT);
                        se.fin_timeout_key = Some(key);
                    }
                    sender = Some(se);
                }

                (SendSig::FinExpired, Some(mut se)) => {
                    let len = Fin.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    let key = timers.insert(DelayEvent::Fin, TIMEOUT);
                    se.fin_timeout_key = Some(key);
                    sender = Some(se);
                }

                (SendSig::Packet(FinOk), Some(mut se)) => {
                    sender = if se.fin_timeout_key.take().is_some() {
                        api_sender_notify_tx.notify_waiters();
                        None
                    } else {
                        Some(se)
                    };
                }

                (SendSig::StartSendingFile(message), None) => {
                    let packet = Init {
                        payload: message.payload_size,
                        transfer: message.payload.len() as u32,
                        name: match &message.kind {
                            MessageKind::File(name) => Some(name),
                            MessageKind::Text => None,
                        },
                    };
                    let len = packet.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    sender = Some(SendState {
                        message,
                        timeout_keys: FxHashMap::default(),
                        next_to_send: SeqNum(0),
                        fin_timeout_key: None,
                    });
                }

                (SendSig::Packet(InitOk), Some(mut se)) => {
                    let send_count = WINDOW_SIZE.min(se.seq_num_count());
                    for left in 0..send_count {
                        let seq_num = SeqNum(left as u32);
                        send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;
                    }
                    se.next_to_send = SeqNum(send_count as u32);

                    sender = Some(se);
                }

                (SendSig::Packet(Nak(seq_num)) | SendSig::AckExpired(seq_num), Some(mut se)) => {
                    send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                    sender = Some(se);
                }

                (_, None) => sender = None,

                (SendSig::StartSendingFile(_), Some(_)) => {
                    panic!("Should not send file in parallel.")
                }
            },

            Sig::Recv(sig) => match (sig, reader) {
                (RecvSig::Packet(Data { seq_num, data }), Some(mut re)) => {
                    // TODO: Nak maybe?
                    debug_assert!(
                        data.len() <= re.payload_size,
                        "Data packet size is bigger then payload_size",
                    );

                    let bytes_before = seq_num.0 as usize * re.payload_size;
                    re.recv_bytes[bytes_before..][..data.len()].copy_from_slice(data);
                    reader = Some(re);

                    // A bit slover version
                    // let mut local_buf = [0; 4];
                    // DataAck(seq_num).serialize(&mut local_buf);
                    // socket.send(&local_buf[..4]).await?;

                    // A bit faster version. It is so ugly that I whould verify it again.
                    buf[0] |= PacketType::DataOk as u8 & !(PacketType::Data as u8);
                    socket.send(&buf[..4]).await?;
                }

                // Receiver
                (RecvSig::Packet(Fin), Some(re)) => {
                    let len = FinOk.serialize(&mut buf);
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

                    reader = None;
                }

                #[rustfmt::skip]
                (RecvSig::Packet(Init { payload, transfer, name }), re) => 'b: {
                    if let Some(r) = re {
                        if !(r.recv_bytes.len() == transfer as usize
                            && r.payload_size == payload as usize
                            && r.name.as_ref().zip(name).map_or(true, |(a, b)| a == b))
                        {
                            reader = Some(r);
                            break 'b;
                        }
                    }

                    let name = name.map(|s| s.to_string());

                    let len = InitOk.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;

                    reader = Some(ReceiveState {
                        recv_bytes: vec![0; transfer as usize],
                        payload_size: payload as usize,
                        name,
                    });
                }

                (RecvSig::Packet(_), None) => reader = None,
            },
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

    let len = Data {
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
