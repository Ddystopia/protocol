use std::io::ErrorKind;
use std::net::SocketAddr;
use std::{io, time::Duration};

use tokio::net::{ToSocketAddrs, UdpSocket};
use tokio::time::{self, Instant};

use crate::packet::Packet;
use crate::{packet::SeqNum, MTU};

mod private {
    #[derive(Debug, Clone, Copy)]
    pub struct PrivateZst;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Handshake(private::PrivateZst);

const TIMEOUT: Duration = Duration::from_millis(100);

#[inline]
pub(crate) async fn handshake_active(
    socket: &UdpSocket,
    addr: impl ToSocketAddrs + Clone,
) -> io::Result<Handshake> {
    socket.connect(addr).await?;
    let mut buf = [0; MTU];

    println!("sending syn");

    let len = Packet::Syn.serialize(&mut buf);
    socket.send(&buf[..len]).await?;

    let mut timeout = Instant::now() + TIMEOUT;

    let seq_num = 'awaiting_for_syn_ack: loop {
        println!("awaiting for syn_ack");

        let timeout_result = time::timeout_at(timeout, socket.recv(&mut buf)).await;
        match timeout_result {
            Ok(Ok(len)) => {
                let Some(packet) = Packet::deserialize(&buf[..len]) else {
                    continue 'awaiting_for_syn_ack;
                };
                if let Packet::SynAck(seq_num) = packet {
                    break 'awaiting_for_syn_ack seq_num;
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) => {
                let syn = Packet::Syn;
                let len = syn.serialize(&mut buf);
                socket.send(&buf[..len]).await?;

                timeout = Instant::now() + TIMEOUT;
            }
        }
    };

    let len = Packet::Ack(seq_num).serialize(&mut buf);
    socket.send(&buf[..len]).await?;

    // NA: event loop should keep responing acks to SYN_ACKs

    Ok(Handshake(private::PrivateZst))
}

#[inline]
pub(crate) async fn handshake_passive(socket: &UdpSocket) -> io::Result<Handshake> {
    'from_scratch: loop {
        let mut buf = [0; MTU];
        let seq_num = SeqNum(5);

        let (seq_num, addr) = 'listening_for_syn: loop {
            println!("listening for syn tick");
            let Ok((len, addr)) = socket.recv_from(&mut buf).await else {
                continue 'listening_for_syn;
            };
            let Some(packet) = Packet::deserialize(&buf[..len]) else {
                continue 'listening_for_syn;
            };

            if let Packet::Syn = packet {
                let syn_ack = Packet::SynAck(seq_num);
                let len = syn_ack.serialize(&mut buf);
                socket.send_to(&buf[..len], addr).await?;
                break 'listening_for_syn (seq_num, addr);
            }
        };

        let mut timeout = Instant::now() + TIMEOUT;

        'awaiting_for_ack: loop {
            println!("Awaiting for ack tick");

            let timeout_resut = time::timeout_at(timeout, socket.recv_from(&mut buf)).await;

            match timeout_resut {
                Ok(Ok((len, a))) => match Packet::deserialize(&buf[..len]) {
                    Some(Packet::Ack(seq_num_i)) if seq_num == seq_num_i && addr == a => {
                        break 'awaiting_for_ack
                    }
                    Some(Packet::Syn) => {
                        let syn_ack = Packet::SynAck(seq_num);
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
                    _ => return Err(e),
                },
                Err(_elapsed) => {
                    let syn_ack = Packet::SynAck(seq_num);
                    let len = syn_ack.serialize(&mut buf);
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
    Packet(Packet, SocketAddr),
    Error(std::io::Error),
    Timeout,
}

#[allow(dead_code)]
async fn handshake_passive_sm(socket: &UdpSocket) -> io::Result<Handshake> {
    use PassiveSignal as Sig;
    use StatePassive as State;

    let mut state = State::Listening;
    let mut at = None;
    let mut buf = [0; MTU];
    let seq_num = SeqNum(5);

    loop {
        let result = match at {
            Some(at) => time::timeout_at(at, socket.recv_from(&mut buf)).await,
            None => Ok(socket.recv_from(&mut buf).await),
        };
        let signal = match result {
            Ok(Ok((len, addr))) => match Packet::deserialize(&buf[..len]) {
                Some(packet) => Sig::Packet(packet, addr),
                None => continue,
            },
            Ok(Err(e)) => Sig::Error(e),
            Err(_elapsed) => Sig::Timeout,
        };

        match (state, signal) {
            (
                State::GotSynSentSynAckWaitingForAck(seq_num, addr),
                Sig::Packet(Packet::Ack(s), a),
            ) => {
                if s != seq_num || a != addr {
                    continue;
                }
                socket.connect(addr).await?;
                return Ok(Handshake(private::PrivateZst));
            }
            (State::GotSynSentSynAckWaitingForAck(_, addr), Sig::Packet(Packet::Syn, a)) |
            (State::Listening, Sig::Packet(Packet::Syn, addr @ a)) |
            (State::GotSynSentSynAckWaitingForAck(_, addr @ a), Sig::Timeout) => if addr == a {
                let syn_ack = Packet::SynAck(seq_num);
                let len = syn_ack.serialize(&mut buf);
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
    use tokio::net::UdpSocket;

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

        let hs2 = tokio::spawn(async move { handshake_passive_sm(&socket2).await });

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
        let hs2 = tokio::spawn(async move { handshake_passive_sm(&socket2).await });

        let (r1, r2) = tokio::join!(hs1, hs2);
        r1.unwrap().unwrap();
        r2.unwrap().unwrap();
    }
}

