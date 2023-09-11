use std::{io, sync::Arc};

use futures::future::pending;
use rustc_hash::FxHashMap;

use tokio::{
    sync::{mpsc, oneshot, Notify},
    time::{timeout_at, Instant},
};
use tokio_util::time::{delay_queue::Key, DelayQueue};

// static RESENT: AtomicU64 = AtomicU64::new(0);

use crate::{
    handshake::Handshake,
    packet::{
        ConnPacket::{self, KeepAlive, KeepAliveOk, Syn, SynAck, SynAckAck},
        PacketType,
        RecvPacket::{self, DataAck, FinOk, InitOk, Nak},
        SendPacket::{self, Data, Fin, FinOkOk, Init},
    },
    packet::{Packet, SeqNum},
    Message, MessageData, ShutdownSignalZST, UdpSocket, ACK_TIMEOUT, KA_TIMEOUT, MSS,
};

const WINDOW_SIZE: usize = 128;

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
    FinOkExpired,
}

#[derive(Debug)]
enum SendSig {
    Packet(RecvPacket),
    InitExpired,
    FinExpired,
    TimeWaitToCloseExpired,
    AckExpired(SeqNum),
    StartSendingFile(Message),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expired {
    Ack(SeqNum),
    Fin,
    FinOk,
    TimeWaitToClose,
    KeepAlive,
    Init,
}

#[derive(Debug)]
struct SendState {
    transfer: Vec<u8>,
    payload_size: u16,
    filename: Option<String>,
    timeout_keys: FxHashMap<SeqNum, Key>,
    next_to_send: SeqNum,
    fin_timeout_key: Option<Key>,
    time_wait_key: Option<Key>,
    init_timeout_key: Option<Key>,
}

impl SendState {
    #[inline]
    fn seq_num_count(&self) -> usize {
        let payload_size = self.payload_size as usize;
        let transfer = self.transfer.len();
        (transfer + payload_size - 1) / payload_size
    }
}

#[derive(Debug)]
struct ReceiveState {
    recv_bytes: Vec<u8>,
    payload_size: u16,
    name: Option<String>,
    fin_ok_timeout_key: Option<Key>,
}

pub(crate) async fn event_loop(
    socket: UdpSocket,
    handshake: Handshake,
    shutdown: oneshot::Receiver<ShutdownSignalZST>,
    api_sender_notify_tx: Arc<Notify>,
    api_sender_rx: mpsc::Receiver<Message>,
    api_received_messages_tx: mpsc::Sender<Message>,
) -> std::io::Result<UdpSocket> {
    Ok(_event_loop(
        socket,
        handshake,
        shutdown,
        api_sender_notify_tx,
        api_sender_rx,
        api_received_messages_tx,
    )
    .await
    .expect("I want to catch all errors early, for better debug experience"))
}
// TODO: Use NAK's somehow
pub(crate) async fn _event_loop(
    socket: UdpSocket,
    _handshake: Handshake,
    mut shutdown: oneshot::Receiver<ShutdownSignalZST>,
    api_sender_notify_tx: Arc<Notify>,
    mut api_sender_rx: mpsc::Receiver<Message>,
    api_received_messages_tx: mpsc::Sender<Message>,
) -> std::io::Result<UdpSocket> {
    let mut buf = [0; MSS];
    let mut reader: Option<ReceiveState> = None;
    let mut sender: Option<SendState> = None;
    let mut timers = DelayQueue::new();
    let mut next_keep_alive = Instant::now() + KA_TIMEOUT;
    let mut keep_alive_key = None;

    'event_loop: loop {
        let delay_event = futures::future::poll_fn(|cx| timers.poll_expired(cx));
        let sig = tokio::select! {
            biased;

            sh_res = &mut shutdown => return match sh_res {
                Ok(ShutdownSignalZST) => Ok(socket),
                Err(_recv_error) => Ok(socket), // Server dropped
            },
            socket_res = socket.recv(&mut buf) => {
                next_keep_alive = Instant::now() + KA_TIMEOUT;
                match socket_res {
                    Ok(len) => match Packet::deserialize(&buf[..len]) {
                        Some(Packet::Conn(p)) => Sig::Conn(ConnSig::Packet(p)),
                        Some(Packet::Send(p)) => Sig::Recv(RecvSig::Packet(p)),
                        Some(Packet::Recv(p)) => Sig::Send(SendSig::Packet(p)),
                        None => continue 'event_loop,
                    }
                    Err(e) => Sig::Conn(ConnSig::SocketError(e))
                }
            },

            _ = timeout_at(next_keep_alive, pending::<()>()) => {
                next_keep_alive = Instant::now() + KA_TIMEOUT;
                Sig::Conn(ConnSig::KeepAlive)
            }

            Some(expired) = delay_event => match expired.into_inner() {
                Expired::Ack(s) => Sig::Send(SendSig::AckExpired(s)),
                Expired::Init => Sig::Send(SendSig::InitExpired),
                Expired::Fin => Sig::Send(SendSig::FinExpired),
                Expired::FinOk => Sig::Recv(RecvSig::FinOkExpired),
                Expired::TimeWaitToClose => Sig::Send(SendSig::TimeWaitToCloseExpired),
                Expired::KeepAlive => Sig::Conn(ConnSig::KeepAliveExpired),
            },

            msg = api_sender_rx.recv() => {
                Sig::Send(SendSig::StartSendingFile(msg.expect("API sender channel closed")))
            },
        };
        // println!("sig: {:?}", sig);

        match sig {
            Sig::Conn(sig) => match sig {
                ConnSig::Packet(Syn | SynAckAck(_)) => (),

                ConnSig::Packet(SynAck(seq_num)) => {
                    let len = ConnPacket::SynAckAck(seq_num).serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                }

                ConnSig::Packet(KeepAlive) => {
                    println!("Got KeepAlive");
                    let len = ConnPacket::KeepAliveOk.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                }

                ConnSig::KeepAliveExpired => {
                    println!("keepalive expired");
                    return Ok(socket);
                }

                ConnSig::KeepAlive => {
                    println!("Sending keepalive",);
                    let len = ConnPacket::KeepAlive.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    let key = timers.insert(Expired::KeepAlive, ACK_TIMEOUT);
                    keep_alive_key = Some(key);
                }

                ConnSig::Packet(KeepAliveOk) => {
                    if let Some(key) = keep_alive_key {
                        timers.remove(&key);
                    }
                }

                ConnSig::SocketError(e) => match e.kind() {
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
                        println!("E: {}", e.kind());
                        return Ok(socket);
                    }
                    io::ErrorKind::ConnectionRefused => {
                        todo!("Idk why but it happens (maybe when active before passive)")
                    }
                    _ => return Err(e),
                },
            },

            Sig::Send(sig) => match (sig, sender) {
                (SendSig::Packet(DataAck(seq_num_ack)), Some(mut se)) => 'block: {
                    se.timeout_keys
                        .remove(&seq_num_ack)
                        .map(|k| timers.remove(&k));

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
                        let key = timers.insert(Expired::Fin, ACK_TIMEOUT);
                        se.fin_timeout_key = Some(key);
                        // let resent = RESENT.load(Relaxed) as f64;
                        // let sent = se.seq_num_count() as f64 + resent;
                        // let fraq = 100. * resent / sent;
                        // println!("File sent. Network Loss: {resent}/{sent} = {fraq:.3}%",);
                    }
                    sender = Some(se);
                }

                (SendSig::AckExpired(seq_num), Some(mut se)) => {
                    se.timeout_keys.remove(&seq_num);
                    send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                    sender = Some(se);
                }

                (SendSig::Packet(Nak(seq_num)), Some(mut se)) => {
                    // TODO: is it correct with timers etc?
                    send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                    sender = Some(se);
                }

                (SendSig::FinExpired, Some(mut se)) => {
                    let len = Fin.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    let key = timers.insert(Expired::Fin, ACK_TIMEOUT);
                    se.fin_timeout_key = Some(key);
                    sender = Some(se);
                }

                (SendSig::TimeWaitToCloseExpired, Some(_)) => {
                    api_sender_notify_tx.notify_waiters();
                    sender = None;
                }

                (SendSig::Packet(FinOk), Some(mut se)) => {
                    let len = FinOkOk.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;

                    se.fin_timeout_key.take().map(|k| timers.remove(&k));
                    // `Some` if FinOkOk got lost and FinOk was resent
                    se.time_wait_key.take().map(|k| timers.remove(&k));
                    let key = Some(timers.insert(Expired::TimeWaitToClose, 10 * ACK_TIMEOUT));
                    se.time_wait_key = key;
                    sender = Some(se);
                }

                (SendSig::StartSendingFile(message), None) => {
                    println!("Start Sending");
                    let (transfer, filename) = match message.data {
                        MessageData::File { name, payload } => (payload, Some(name)),
                        MessageData::Text(text) => (text.into_bytes(), None),
                    };

                    let packet = Init {
                        payload_size: message.payload_size,
                        transfer: transfer.len() as u32,
                        name: filename.as_deref(),
                    };

                    let len = packet.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    sender = Some(SendState {
                        transfer,
                        filename,
                        payload_size: message.payload_size,
                        timeout_keys: FxHashMap::default(),
                        next_to_send: SeqNum(0),
                        fin_timeout_key: None,
                        time_wait_key: None,
                        init_timeout_key: Some(timers.insert(Expired::Init, ACK_TIMEOUT)),
                    });
                    assert!(
                        sender.as_ref().unwrap().seq_num_count() & 0xF000_0000 == 0,
                        "Too many packets"
                    );
                }
                (SendSig::InitExpired, Some(mut se)) => {
                    let len = Init {
                        payload_size: se.payload_size,
                        name: se.filename.as_deref(),
                        transfer: se.transfer.len() as u32,
                    }
                    .serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    let key = timers.insert(Expired::Init, ACK_TIMEOUT);
                    se.init_timeout_key = Some(key);
                    sender = Some(se);
                }

                (SendSig::Packet(InitOk), Some(mut se)) => {
                    se.init_timeout_key.take().map(|k| timers.remove(&k));
                    let send_count = WINDOW_SIZE.min(se.seq_num_count());
                    for left in 0..send_count {
                        let seq_num = SeqNum(left as u32);
                        send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;
                    }
                    se.next_to_send = SeqNum(send_count as u32);

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
                        data.len() <= re.payload_size.into(),
                        "Data packet size is bigger then payload_size",
                    );

                    let bytes_before = seq_num.0 as usize * re.payload_size as usize;
                    re.recv_bytes[bytes_before..][..data.len()].copy_from_slice(data);
                    reader = Some(re);

                    // // A slover version
                    // let mut local_buf = [0; 4];
                    // DataAck(seq_num).serialize(&mut local_buf);
                    // socket.send(&local_buf[..4]).await?;

                    // A faster version.
                    buf[0] |= PacketType::DataOk as u8 & !(PacketType::Data as u8);
                    #[cfg(debug_assertions)]
                    if true {
                        let mut local_buf = [0; 4];
                        DataAck(seq_num).serialize(&mut local_buf);
                        assert_eq!(local_buf[..4], buf[..4], "Verify hask is correnct.");
                    }
                    socket.send(&buf[..4]).await?;
                }

                (s @ (RecvSig::Packet(Fin) | RecvSig::FinOkExpired), Some(mut re)) => {
                    if matches!(s, RecvSig::Packet(Fin)) {
                        // `Some` if `FinOk` got lost and Sender resent `Fin`
                        re.fin_ok_timeout_key.take().map(|k| timers.remove(&k));
                    }
                    let len = FinOk.serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    re.fin_ok_timeout_key = Some(timers.insert(Expired::FinOk, ACK_TIMEOUT));

                    reader = Some(re);
                }

                (RecvSig::Packet(FinOkOk), Some(mut re)) => {
                    match re.fin_ok_timeout_key.take() {
                        Some(k) => timers.remove(&k),
                        None => panic!("FinOkOk arrived before FinOk"),
                    };

                    let message = Message {
                        payload_size: re.payload_size,
                        data: match re.name {
                            Some(name) => MessageData::File {
                                name,
                                payload: re.recv_bytes,
                            },
                            None => MessageData::Text(
                                String::from_utf8(re.recv_bytes).expect("Should be valid utf-8"),
                            ),
                        },
                    };

                    api_received_messages_tx
                        .send(message)
                        .await
                        .expect("TODO: convert to io error");

                    reader = None;
                }

                #[rustfmt::skip]
                (RecvSig::Packet(Init { payload_size, transfer, name }), re) => 'b: {
                    println!("Init packet arrived");
                    if let Some(r) = re {
                        if !(r.recv_bytes.len() == transfer as usize
                            && r.payload_size == payload_size
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
                        payload_size,
                        name,
                        fin_ok_timeout_key: None
                    });
                }

                (_, None) => reader = None,
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
    timers: &mut DelayQueue<Expired>,
) -> std::io::Result<()> {
    let payload_size = sender.payload_size as usize;
    let bytes_before = seq_num.0 as usize * payload_size;

    debug_assert!(bytes_before < sender.transfer.len(), "Too many bytes sent.");

    let next_bytes = &sender.transfer[bytes_before..];

    let len = Data {
        seq_num,
        data: &next_bytes[..payload_size.min(next_bytes.len())],
    }
    .serialize(buf);
    socket.send(&buf[..len]).await?;

    let key = timers.insert(Expired::Ack(seq_num), ACK_TIMEOUT);

    if let Some(removed_key) = sender.timeout_keys.insert(seq_num, key) {
        timers.remove(&removed_key);
    }

    std::io::Result::Ok(())
}
