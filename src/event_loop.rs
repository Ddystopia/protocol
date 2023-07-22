use std::{collections::HashMap, io::ErrorKind, sync::Arc};

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
    time::Instant,
};
use tokio_util::time::{delay_queue::Key, DelayQueue};

use crate::Message;

#[derive(Debug)]
enum Sig<'a> {
    Packet(Packet<'a>),
    Timeout(DelayEvent),
    SocketError(std::io::Error),
    StartSendingFile(Message),
}

#[derive(Debug)]
enum DelayEvent {
    AckTimeout(SeqNum),
}

#[derive(Debug, PartialEq, Eq, Hash)]
enum SenderTimeoutKeys {
    SeqNum(SeqNum),
    Fin,
}

struct SendState {
    message: Message,
    timeout_keys: HashMap<SenderTimeoutKeys, Key>,
    next_to_send: SeqNum,
}

impl SendState {
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

//  TODO: keep sending Ack to SeqAcks
pub(crate) async fn event_loop(
    socket: UdpSocket,
    _handshake: Handshake,
    shutdown: Arc<Notify>,
    api_sender_notify_tx: Arc<Notify>,
    mut api_sender_rx: mpsc::Receiver<Message>,
    api_received_messages_tx: mpsc::Sender<Message>,
) -> std::io::Result<UdpSocket> {
    // Invariant: key should be stored somewhere, but only if it is valid
    let mut timers = DelayQueue::new();
    let mut buf = [0; MTU];
    let mut reader: Option<ReceiveState> = None;
    let mut sender: Option<SendState> = None;

    'event_loop: loop {
        println!("Event loop tick");
        let delay_event = futures::future::poll_fn(|cx| timers.poll_expired(cx));
        let sig = async {
            tokio::select! {
                Some(expired) = delay_event => Some(Sig::Timeout(expired.into_inner())),
                socket_res = socket.recv(&mut buf) => Some(match socket_res {
                    Ok(len) => Sig::Packet(Packet::deserialize(&buf[..len])?),
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

        (reader, sender) = match (sig, reader, sender) {
            (Sig::Packet(Syn), r, s) => (r, s),
            (Sig::Packet(SynAck(seq_num)), r, s) => {
                let len = Packet::Ack(seq_num).serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (r, s)
            }
            (Sig::StartSendingFile(_), _, Some(_)) => panic!("Should not sent file in parallel."),
            (Sig::Packet(KeepAlive), r, s) => {
                let len = Packet::KeepAliveOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (r, s)
            }
            (Sig::Packet(KeepAliveOk), _r, _s) => todo!("When send of keepalive implemented"),
            (Sig::Packet(Fin | FinOk | Ack(_) | Nak(_)), r, None) => (r, None),
            (Sig::Packet(Data { .. } | Fin), None, s) => (None, s),

            // Receiver
            (Sig::Packet(Fin), Some(re), s) => {
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
                let reader = Some(ReceiveState {
                    recv_bytes: vec![0; transfer as usize], // TODO: bench MaybeUninit
                    payload_size: payload as usize,
                    name: name.map(|s| s.to_string()),
                });
                let len = Packet::InitOk.serialize(&mut buf);
                socket.send(&buf[..len]).await?;
                (reader, s)
            }

            (Sig::Packet(Data { seq_num, data }), Some(mut re), s) => {
                let payload_size = re.payload_size;
                // TODO: Nak maybe?
                assert!(
                    data.len() == payload_size,
                    "Only payload size should be sent",
                );
                let bytes_before = seq_num.0 as usize * payload_size;
                re.recv_bytes[bytes_before..][..data.len()].copy_from_slice(data);
                (Some(re), s)
            }
            (Sig::SocketError(e), _, _) => match e.kind() {
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => return Ok(socket),
                _ => return Err(e),
            },
            (Sig::Timeout(_), _re, None) => {
                todo!("After Receiver & KeepAlive, idk is timer needed for it")
            }

            // Sender
            (Sig::Packet(FinOk), r, Some(mut se)) => {
                if se.timeout_keys.remove(&SenderTimeoutKeys::Fin).is_some() {
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
                });
                (r, sender)
            }

            (Sig::Packet(InitOk), _, None) => todo!("Got InitOk when None? Why?"),
            (Sig::Packet(InitOk), r, Some(mut se)) => {
                let send_count = WINDOW_SIZE.min(se.seq_num_count());
                for left in 0..send_count {
                    let seq_num = SeqNum(left as u32);
                    send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;
                }
                se.next_to_send = SeqNum(send_count as u32);

                (r, Some(se))
            }

            (Sig::Packet(Ack(seq_num_ack)), r, Some(mut se)) => 'block: {
                // maybe panic?
                let seq_key = SenderTimeoutKeys::SeqNum(seq_num_ack);
                if let Some(key) = se.timeout_keys.remove(&seq_key) {
                    timers.remove(&key);
                }

                let seq_num = se.next_to_send;

                if seq_num == SeqNum(se.seq_num_count() as u32) {
                    // What if NAK ?
                    // Maybe introduce FIN packet?
                    // Is race possible?
                    break 'block if se.timeout_keys.is_empty() {
                        (r, None)
                    } else {
                        (r, Some(se))
                    };
                }

                se.next_to_send += SeqNum(1);

                send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                (r, Some(se))
            }

            (
                Sig::Packet(Nak(seq_num)) | Sig::Timeout(DelayEvent::AckTimeout(seq_num)),
                r,
                Some(mut se),
            ) => {
                send_data_packet(&mut se, &socket, &mut buf, seq_num, &mut timers).await?;

                (r, Some(se))
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

    assert!(bytes_before < msg.payload.len(), "Too many bytes sent.");

    let len = Packet::Data {
        seq_num,
        data: &msg.payload[bytes_before..][..payload_size],
    }
    .serialize(buf);
    socket.send(&buf[..len]).await?;

    let delay = DelayEvent::AckTimeout(seq_num);
    let key = timers.insert_at(delay, Instant::now() + TIMEOUT);

    let seq = SenderTimeoutKeys::SeqNum(seq_num);
    if let Some(key) = sender.timeout_keys.insert(seq, key) {
        timers.remove(&key);
    }
    std::io::Result::Ok(())
}
