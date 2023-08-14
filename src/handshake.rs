use std::io::{self, ErrorKind};
use std::net::SocketAddr;

use tokio::time::{self, Instant};

use crate::UdpSocket;
use crate::ToSocketAddrs;
use crate::{
    packet::{ConnPacket, Packet, SeqNum},
    MSS, TIMEOUT,
};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub struct PrivateZst;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Handshake(private::PrivateZst);

#[inline]
pub(crate) async fn handshake_active(
    socket: &UdpSocket,
    addr: impl ToSocketAddrs,
) -> io::Result<Handshake> {
    socket.connect(addr).await?;
    let mut buf = [0; MSS];

    let len = ConnPacket::Syn.serialize(&mut buf);
    socket.send(&buf[..len]).await?;

    let mut timeout = Instant::now() + TIMEOUT;

    let seq_num = 'awaiting_for_syn_ack: loop {
        let timeout_result = time::timeout_at(timeout, socket.recv(&mut buf)).await;
        match timeout_result {
            Ok(Ok(len)) => {
                let Some(packet) = Packet::deserialize(&buf[..len]) else {
                    continue 'awaiting_for_syn_ack;
                };
                if let Packet::Conn(ConnPacket::SynAck(seq_num)) = packet {
                    break 'awaiting_for_syn_ack seq_num;
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) => {
                let len = ConnPacket::Syn.serialize(&mut buf);
                socket.send(&buf[..len]).await?;

                timeout = Instant::now() + TIMEOUT;
            }
        }
    };

    let len = ConnPacket::SynAckAck(seq_num).serialize(&mut buf);
    socket.send(&buf[..len]).await?;

    // NA: event loop should keep responing acks to SYN_ACKs

    Ok(Handshake(private::PrivateZst))
}

#[inline]
pub(crate) async fn handshake_passive(socket: &UdpSocket) -> io::Result<Handshake> {
    'from_scratch: loop {
        let mut buf = [0; MSS];
        let seq_num = SeqNum(4);

        let (seq_num, addr) = 'listening_for_syn: loop {
            let Ok((len, addr)) = socket.recv_from(&mut buf).await else {
                continue 'listening_for_syn;
            };
            let Some(packet) = Packet::deserialize(&buf[..len]) else {
                continue 'listening_for_syn;
            };

            if let Packet::Conn(ConnPacket::Syn) = packet {
                let syn_ack = ConnPacket::SynAck(seq_num);
                let len = syn_ack.serialize(&mut buf);
                socket.send_to(&buf[..len], addr).await?;
                break 'listening_for_syn (seq_num, addr);
            }
        };

        let mut timeout = Instant::now() + TIMEOUT;

        'awaiting_for_ack: loop {
            let timeout_resut = time::timeout_at(timeout, socket.recv_from(&mut buf)).await;

            match timeout_resut {
                Ok(Ok((len, a))) => match Packet::deserialize(&buf[..len]) {
                    Some(Packet::Conn(ConnPacket::SynAckAck(seq_num_i)))
                        if seq_num == seq_num_i && addr == a =>
                    {
                        break 'awaiting_for_ack
                    }
                    Some(Packet::Conn(ConnPacket::Syn)) => {
                        let syn_ack = ConnPacket::SynAck(seq_num);
                        let len = syn_ack.serialize(&mut buf);
                        socket.send_to(&buf[..len], addr).await?;
                        continue 'awaiting_for_ack;
                    }
                    _ => continue 'awaiting_for_ack,
                },
                Ok(Err(e)) => match e.kind() {
                    ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
                        continue 'from_scratch
                    }
                    ErrorKind::ConnectionRefused => todo!("Idk why but it happens"),
                    _ => return Err(e),
                },
                Err(_elapsed) => {
                    let len = ConnPacket::SynAck(seq_num).serialize(&mut buf);
                    socket.send(&buf[..len]).await?;
                    timeout = Instant::now() + TIMEOUT;
                }
            }
        }

        socket.connect(addr).await?;
        return Ok(Handshake(private::PrivateZst));
    }
}

#[derive(Debug, Clone, Copy)]
enum StatePassive {
    Listening,
    GotSynSentSynAckWaitingForAck(SeqNum, SocketAddr),
}

#[derive(Debug)]
enum PassiveSignal {
    Packet(ConnPacket, SocketAddr),
    Error(std::io::Error),
    Timeout,
}

#[allow(dead_code)]
pub(crate) async fn handshake_passive_sm(socket: &UdpSocket) -> io::Result<Handshake> {
    use PassiveSignal as Sig;
    use StatePassive as State;

    let mut state = State::Listening;
    let mut at = None;
    let mut buf = [0; MSS];
    let seq_num = SeqNum(5);

    loop {
        let result = match at {
            Some(at) => time::timeout_at(at, socket.recv_from(&mut buf)).await,
            None => Ok(socket.recv_from(&mut buf).await),
        };
        let signal = match result {
            Ok(Ok((len, addr))) => match Packet::deserialize(&buf[..len]) {
                Some(Packet::Conn(packet)) => Sig::Packet(packet, addr),
                _ => continue,
            },
            Ok(Err(e)) => Sig::Error(e),
            Err(_elapsed) => Sig::Timeout,
        };

        match (state, signal) {
            (
                State::GotSynSentSynAckWaitingForAck(seq_num, addr),
                Sig::Packet(ConnPacket::SynAckAck(s), a),
            ) => {
                if s != seq_num || a != addr {
                    continue;
                }
                socket.connect(addr).await?;
                return Ok(Handshake(private::PrivateZst));
            }
            (State::GotSynSentSynAckWaitingForAck(_, addr), Sig::Packet(ConnPacket::Syn, a)) |
            (State::Listening, Sig::Packet(ConnPacket::Syn, addr @ a)) |
            (State::GotSynSentSynAckWaitingForAck(_, addr @ a), Sig::Timeout) => if addr == a {
                let len = ConnPacket::SynAck(seq_num).serialize(&mut buf);
                socket.send_to(&buf[..len], addr).await?;
                at = Some(Instant::now() + TIMEOUT);
                state = State::GotSynSentSynAckWaitingForAck(seq_num, addr);
            },

            (State::Listening, Sig::Error(e)) => return Err(e),
            (State::GotSynSentSynAckWaitingForAck(_, _), Sig::Error(e)) => match e.kind() {
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
                    at = None;
                    state = State::Listening;
                },
                ErrorKind::ConnectionRefused => todo!("Idk why but it happens"),
                _ => return Err(e),
            }

            // Got not Ack
            (State::GotSynSentSynAckWaitingForAck(..), Sig::Packet(..)) |
            // Got not Syn
            (State::Listening, Sig::Packet(_,_)) => continue,
            (State::Listening, Sig::Timeout) => {
                panic!("Should not get any timeout while listening. Maybe you forgot to clean it?")
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::time::Duration;
    use crate::UdpSocket;

    #[tokio::test]
    async fn connection_establishment() {
        const ADDR1: &str = "127.0.0.1:10101";
        const ADDR2: &str = "127.0.0.1:10102";

        let socket1 = UdpSocket::bind(ADDR1).await.unwrap();
        let socket2 = UdpSocket::bind(ADDR2).await.unwrap();

        let delay_for_active = time::sleep(Duration::from_millis(100));

        let hs1 = tokio::spawn(async move {
            delay_for_active.await;
            handshake_active(&socket1, ADDR2.clone()).await
        });

        let hs2 = tokio::spawn(async move { handshake_passive(&socket2).await });
        // let hs2 = tokio::spawn(async move { handshake_passive_sm(&socket2).await });

        let (r1, r2) = tokio::join!(hs1, hs2);
        r1.unwrap().unwrap();
        r2.unwrap().unwrap();
    }

    #[tokio::test]
    async fn racy() {
        const ADDR1: &str = "127.0.0.1:10105";
        const ADDR2: &str = "127.0.0.1:10106";

        let socket1 = UdpSocket::bind(ADDR1).await.unwrap();
        let socket2 = UdpSocket::bind(ADDR2).await.unwrap();

        let hs1 = tokio::spawn(async move { handshake_active(&socket1, ADDR2).await });
        // let hs2 = tokio::spawn(async move { handshake_passive(&socket2).await });
        let hs2 = tokio::spawn(async move { handshake_passive_sm(&socket2).await });

        let (r1, r2) = tokio::join!(hs1, hs2);
        r1.unwrap().unwrap();
        r2.unwrap().unwrap();
    }
}
