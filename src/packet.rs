use std::{
    fmt::{Debug, Formatter},
    num::Wrapping,
    ops::AddAssign,
    str::Utf8Error,
};

use crc::{Crc, CRC_32_ISCSI};

use crate::{CRC_BYTES, MSS};

const CASTAGNOLI: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeqNum(Wrapping<u32>);

impl SeqNum {
    #[inline]
    pub const fn new(n: u32) -> Self {
        Self(Wrapping(n))
    }
    #[inline]
    pub fn get(self) -> u32 {
        self.0 .0
    }
}

impl AddAssign for SeqNum {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet<'a> {
    Conn(ConnPacket),
    Send(SendPacket<'a>),
    Recv(RecvPacket),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConnPacket {
    Syn,
    SynAck(SeqNum),
    SynAckAck(SeqNum),
    KeepAlive,
    KeepAliveOk,
}

/// Packet that Sender sends to Receiver
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum SendPacket<'a> {
    Init {
        payload_size: u16,
        transfer: u32,
        name: Option<&'a str>,
    },
    Data {
        seq_num: SeqNum,
        data: &'a [u8],
    },
    Fin,
    FinOkOk,
}

impl Debug for SendPacket<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SendPacket::Init {
                payload_size,
                transfer,
                name,
            } => f
                .debug_struct("SendPacket::Init")
                .field("payload_size", payload_size)
                .field("transfer", transfer)
                .field("name", name)
                .finish(),
            SendPacket::Data { seq_num, data } => f
                .debug_struct("SendPacket::Data")
                .field("seq_num", seq_num)
                .field("data.len", &data.len())
                .finish(),
            SendPacket::Fin => f.debug_struct("SendPacket::Fin").finish(),
            SendPacket::FinOkOk => f.debug_struct("SendPacket::FinOkOk").finish(),
        }
    }
}

/// Packet that Receiver sends to Sender
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RecvPacket {
    DataAck(SeqNum),
    Nak(SeqNum),
    InitOk,
    FinOk,
}

// IMPORTANT: When adding new packet types, update `assert_constant`
//            function to include new one
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PacketType {
    Syn = 0b0111 << 4,
    SynAck = 0b1000 << 4,
    SynAckAck = 0b1011 << 4,
    Init = 0b0000 << 4,
    Data = 0b0001 << 4,
    DataOk = 0b0101 << 4,
    Nak = 0b0110 << 4,
    KeepAlive = 0b0010 << 4,
    InitOk = 0b0011 << 4,
    KeepAliveOk = 0b0100 << 4,
    Fin = 0b1001 << 4,
    FinOk = 0b1010 << 4,
    FinOkOk = 0b1100 << 4,
}

impl PacketType {
    const DISCRIMINANT_MASK: u8 = 0xF0;

    const SYN: u8 = Self::Syn as u8;
    const SYN_ACK: u8 = Self::SynAck as u8;
    const SYN_ACK_ACK: u8 = Self::SynAckAck as u8;
    const INIT: u8 = Self::Init as u8;
    const DATA: u8 = Self::Data as u8;
    const DATA_OK: u8 = Self::DataOk as u8;
    const NAK: u8 = Self::Nak as u8;
    const KEEPALIVE: u8 = Self::KeepAlive as u8;
    const INIT_OK: u8 = Self::InitOk as u8;
    const KEEPALIVE_OK: u8 = Self::KeepAliveOk as u8;
    const FIN: u8 = Self::Fin as u8;
    const FIN_OK: u8 = Self::FinOk as u8;
    const FIN_OK_OK: u8 = Self::FinOkOk as u8;
}

impl TryFrom<u8> for PacketType {
    type Error = ();
    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        try_from(value)
    }
}

#[inline]
const fn try_from(value: u8) -> Result<PacketType, ()> {
    match value {
        PacketType::SYN => Ok(PacketType::Syn),
        PacketType::SYN_ACK => Ok(PacketType::SynAck),
        PacketType::SYN_ACK_ACK => Ok(PacketType::SynAckAck),
        PacketType::INIT => Ok(PacketType::Init),
        PacketType::DATA => Ok(PacketType::Data),
        PacketType::DATA_OK => Ok(PacketType::DataOk),
        PacketType::NAK => Ok(PacketType::Nak),
        PacketType::KEEPALIVE => Ok(PacketType::KeepAlive),
        PacketType::INIT_OK => Ok(PacketType::InitOk),
        PacketType::KEEPALIVE_OK => Ok(PacketType::KeepAliveOk),
        PacketType::FIN => Ok(PacketType::Fin),
        PacketType::FIN_OK => Ok(PacketType::FinOk),
        PacketType::FIN_OK_OK => Ok(PacketType::FinOkOk),
        _ => Err(()),
    }
}

#[inline]
fn string_from_null_terminated(buf: &[u8; 22]) -> Result<Option<&str>, Utf8Error> {
    let name_end = buf.iter().take(22).position(|&x| x == 0).unwrap_or(22);
    if name_end == 0 {
        return Ok(None);
    }
    std::str::from_utf8(&buf[..name_end]).map(Some)
}

impl<'a> SendPacket<'a> {
    #[inline]
    pub fn serialize(&self, buf: &mut [u8; MSS]) -> usize {
        Packet::Send(*self).serialize(buf)
    }
}

impl RecvPacket {
    #[inline]
    pub fn serialize(self, buf: &mut [u8; MSS]) -> usize {
        Packet::Recv(self).serialize(buf)
    }
}

impl ConnPacket {
    #[inline]
    pub fn serialize(self, buf: &mut [u8; MSS]) -> usize {
        Packet::Conn(self).serialize(buf)
    }
}

#[derive(Debug)]
pub(crate) struct CorruptedPacket(pub SeqNum);

impl<'a> Packet<'a> {
    /// Parses 28 bytes u32 like that
    ///
    /// |0123456789abcdef|
    /// |----------------|
    /// |0000ssssssssssss|
    /// |ssssssssssssssss|
    ///
    #[inline]
    fn decode_sequence(bytes: &[u8]) -> Option<SeqNum> {
        if bytes.len() < 4 {
            debug_assert!(bytes.len() >= 4, "Packet is malformed");
            return None;
        }
        let bytes: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let mask = (PacketType::DISCRIMINANT_MASK as u32) << 24;
        Some(SeqNum::new(u32::from_be_bytes(bytes) & !mask))
    }

    #[inline]
    fn encode_sequence(sequence: SeqNum, buf: &mut [u8]) {
        buf.copy_from_slice(&sequence.0 .0.to_be_bytes());
    }

    #[inline]
    pub(crate) fn deserialize(bytes: &'a [u8]) -> Result<Option<Self>, CorruptedPacket> {
        debug_assert!(bytes.len() > CRC_BYTES, "Packet is malformed");

        let (c, bytes) = bytes.split_at(CRC_BYTES);
        let checksum = u32::from_le_bytes(c.try_into().expect("bytes.get()?"));
        if CASTAGNOLI.checksum(bytes) != checksum {
            let Some(b) = bytes.first() else {
                return Ok(None);
            };

            let discriminant = b & PacketType::DISCRIMINANT_MASK;

            return if discriminant == PacketType::Data as u8 {
                Err(CorruptedPacket(Self::decode_sequence(bytes).unwrap()))
            } else {
                Ok(None)
            };
        }

        Ok(Self::deserialize_inner(bytes))
    }

    #[inline]
    fn deserialize_inner(bytes: &'a [u8]) -> Option<Self> {
        use PacketType::{
            Data, DataOk, Fin, FinOk, FinOkOk, Init, InitOk, KeepAlive, KeepAliveOk, Nak, Syn,
            SynAck, SynAckAck,
        };

        let discriminant = bytes.first()? & PacketType::DISCRIMINANT_MASK;
        let ptype = PacketType::try_from(discriminant).ok()?;

        let packet = match ptype {
            Data => Self::Send(SendPacket::Data {
                seq_num: Self::decode_sequence(bytes)?,
                data: &bytes[4..],
            }),
            DataOk => Self::Recv(RecvPacket::DataAck(Self::decode_sequence(bytes)?)),
            Nak => Self::Recv(RecvPacket::Nak(Self::decode_sequence(bytes)?)),
            KeepAlive => Self::Conn(ConnPacket::KeepAlive),
            InitOk => Self::Recv(RecvPacket::InitOk),
            KeepAliveOk => Self::Conn(ConnPacket::KeepAliveOk),
            Syn => Self::Conn(ConnPacket::Syn),
            SynAck => Self::Conn(ConnPacket::SynAck(Self::decode_sequence(bytes)?)),
            SynAckAck => Self::Conn(ConnPacket::SynAckAck(Self::decode_sequence(bytes)?)),
            Fin => Self::Send(SendPacket::Fin),
            FinOk => Self::Recv(RecvPacket::FinOk),
            FinOkOk => Self::Send(SendPacket::FinOkOk),
            Init => {
                debug_assert!(bytes.len() == 28, "Init packet is malformed");
                let bytes: &[u8; 28] = bytes.try_into().ok()?;
                let first = bytes[0] & !PacketType::DISCRIMINANT_MASK;
                let payload_size = ((first as u16) << 7) | ((bytes[1] as u16) >> 1);
                let transfer_size = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Self::Send(SendPacket::Init {
                    payload_size,
                    transfer: transfer_size,
                    name: string_from_null_terminated(bytes[6..28].try_into().unwrap()).ok()?,
                })
            }
        };
        Some(packet)
    }

    #[inline]
    pub fn serialize(&self, buf: &mut [u8; MSS]) -> usize {
        let (checksum, buf) = buf.split_at_mut(CRC_BYTES);

        match self {
            Self::Send(SendPacket::Init {
                payload_size,
                transfer: _,
                name,
            }) => {
                let ps = payload_size & 0b1111_1000_0000_0000 == 0;
                let len = name.map(|n| n.bytes().len()).unwrap_or(0) <= 22;
                debug_assert!(ps, "Payload size should have 5 high bits zero");
                debug_assert!(len, "Name should not be greater then 22 bytes");
            }
            Self::Send(SendPacket::Data { seq_num, data: _ })
            | Self::Recv(RecvPacket::DataAck(seq_num) | RecvPacket::Nak(seq_num))
            | Self::Conn(ConnPacket::SynAckAck(seq_num) | ConnPacket::SynAck(seq_num)) => {
                let seq = seq_num.0 .0 & 0xF0_00_00_00 == 0;
                debug_assert!(seq, "Seq num should have 4 high bits zero");
            }

            Self::Send(SendPacket::Fin | SendPacket::FinOkOk) | Self::Conn(_) | Self::Recv(_) => {}
        }

        let result_len = match *self {
            Self::Send(SendPacket::Init {
                payload_size,
                transfer: transfer_size,
                name,
            }) => {
                buf[0] = (payload_size >> 7) as u8;
                buf[1] = ((payload_size as u8) & 0b0111_1111) << 1;
                buf[2..6].copy_from_slice(&transfer_size.to_le_bytes());
                buf[6..][..22].fill(0);
                if let Some(name) = name.map(|n| n.as_bytes()) {
                    buf[6..][..name.len()].copy_from_slice(name);
                }

                28
            }
            Self::Send(SendPacket::Data { seq_num, data }) => {
                Self::encode_sequence(seq_num, &mut buf[0..4]);
                buf[4..][..data.len()].copy_from_slice(data);
                4 + data.len()
            }
            Self::Conn(ConnPacket::SynAckAck(seq_num) | ConnPacket::SynAck(seq_num))
            | Self::Recv(RecvPacket::DataAck(seq_num) | RecvPacket::Nak(seq_num)) => {
                Self::encode_sequence(seq_num, &mut buf[0..4]);
                4
            }
            Self::Send(SendPacket::Fin | SendPacket::FinOkOk)
            | Self::Recv(RecvPacket::FinOk | RecvPacket::InitOk)
            | Self::Conn(ConnPacket::Syn | ConnPacket::KeepAlive | ConnPacket::KeepAliveOk) => 1,
        };

        let discriminant = match self {
            Self::Send(SendPacket::Init { .. }) => PacketType::Init,
            Self::Send(SendPacket::Data { .. }) => PacketType::Data,
            Self::Recv(RecvPacket::DataAck(..)) => PacketType::DataOk,
            Self::Recv(RecvPacket::Nak(..)) => PacketType::Nak,
            Self::Conn(ConnPacket::KeepAlive) => PacketType::KeepAlive,
            Self::Recv(RecvPacket::InitOk) => PacketType::InitOk,
            Self::Conn(ConnPacket::KeepAliveOk) => PacketType::KeepAliveOk,
            Self::Conn(ConnPacket::Syn) => PacketType::Syn,
            Self::Conn(ConnPacket::SynAck(..)) => PacketType::SynAck,
            Self::Conn(ConnPacket::SynAckAck(..)) => PacketType::SynAckAck,
            Self::Send(SendPacket::Fin) => PacketType::Fin,
            Self::Recv(RecvPacket::FinOk) => PacketType::FinOk,
            Self::Send(SendPacket::FinOkOk) => PacketType::FinOkOk,
        };

        buf[0] &= !PacketType::DISCRIMINANT_MASK;
        buf[0] |= discriminant as u8;

        let c = CASTAGNOLI.checksum(&buf[..result_len]).to_le_bytes();
        checksum.copy_from_slice(&c);

        result_len + CRC_BYTES
    }
}

#[allow(dead_code)]
const ASSERT_CONSTANTS: () = assert_constants();

const fn assert_constants() {
    const DISCRIMINANTS: [PacketType; 13] = [
        PacketType::Syn,
        PacketType::SynAck,
        PacketType::SynAckAck,
        PacketType::Init,
        PacketType::Data,
        PacketType::KeepAlive,
        PacketType::InitOk,
        PacketType::KeepAliveOk,
        PacketType::DataOk,
        PacketType::Nak,
        PacketType::Fin,
        PacketType::FinOk,
        PacketType::FinOkOk,
    ];
    const LEN: usize = DISCRIMINANTS.len();
    let mut i = 0;

    while i < LEN {
        assert!(
            match try_from(DISCRIMINANTS[i] as u8) {
                Ok(d) => d as u8 == DISCRIMINANTS[i] as u8,
                Err(()) => panic!("Caramba!"),
            },
            "Try from should work fine."
        );
        assert!(
            DISCRIMINANTS[i] as u8 & !PacketType::DISCRIMINANT_MASK == 0,
            "Discriminant is wrong"
        );
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_decodings() {
        let check = |n| {
            let mut buf = [0; 4];
            Packet::encode_sequence(SeqNum::new(n), &mut buf);
            assert_eq!(buf[0] & PacketType::DISCRIMINANT_MASK, 0, "Test malformed");
            assert_eq!(Some(SeqNum::new(n)), Packet::decode_sequence(&buf));
        };
        check(0x0F_FF_FF_0F);
        check(0x0A_BB_CC_0F);
        check(0x01_23_45_0F);
    }

    #[test]
    fn test_parse_init() {
        let mut bytes = [0u8; MSS];
        bytes[0..6].copy_from_slice(&[
            0b0000_1000,
            0b1000_0000,
            // revesed because of endianness
            0b1000_1111,
            0b0110_0011,
            0b0000_0101,
            0b0000_0000,
        ]);
        bytes[6..11].copy_from_slice(b"hello");
        let packet = Packet::deserialize(&bytes[..28]).unwrap().unwrap();
        if let Packet::Send(SendPacket::Init {
            payload_size,
            transfer: transfer_size,
            name,
        }) = packet
        {
            assert_eq!(payload_size, 0b100_0100_0000);
            assert_eq!(name.unwrap(), "hello");
            assert_eq!(transfer_size, 0b0000_0000_0000_0101_0110_0011_1000_1111);
        } else {
            panic!("Unexpected packet type");
        }
    }

    #[test]
    fn test_parse_data() {
        let mut bytes = [0u8; 9];
        bytes[0..4].copy_from_slice(&[0b0001_0000, 0b0000_0000, 0b1000_0000, 0b1111_1111]);
        bytes[4..9].copy_from_slice(b"data!");
        let packet = Packet::deserialize(&bytes).unwrap().unwrap();
        if let Packet::Send(SendPacket::Data {
            seq_num: sequence,
            data,
        }) = packet
        {
            assert_eq!(sequence.0 .0, 0b0000_1000_0000_1111_1111);
            assert_eq!(data, b"data!");
        } else {
            panic!("Unexpected packet type");
        }
    }

    #[test]
    fn test_init_packet() {
        let packet = Packet::Send(SendPacket::Init {
            payload_size: 512,
            transfer: 10000,
            name: Some("testfile"),
        });

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf[..28]).unwrap().unwrap();

        assert_eq!(dbg!(packet), dbg!(received_packet));
    }

    #[test]
    fn test_data_packet() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = [0; MSS];

        let packet = SendPacket::Data {
            seq_num: SeqNum::new(5),
            data: &data,
        };

        let len = packet.serialize(&mut buf);
        let Packet::Send(received_packet) = Packet::deserialize(&buf[..len]).unwrap().unwrap()
        else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ack_packet() {
        let packet = RecvPacket::DataAck(SeqNum::new(10));

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let Packet::Recv(received_packet) = Packet::deserialize(&buf).unwrap().unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_nack_packet() {
        let packet = RecvPacket::Nak(SeqNum::new(20));

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let Packet::Recv(received_packet) = Packet::deserialize(&buf).unwrap().unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }
    #[test]
    fn test_syn_ack_ack_packet() {
        let packet = ConnPacket::SynAckAck(SeqNum::new(10));

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let Packet::Conn(received_packet) = Packet::deserialize(&buf).unwrap().unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_keep_alive_packet() {
        let packet = Packet::Conn(ConnPacket::KeepAlive);

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap().unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_init_ok_packet() {
        let packet = Packet::Recv(RecvPacket::InitOk);

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap().unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ka_ok_packet() {
        let packet = Packet::Conn(ConnPacket::KeepAliveOk);

        let mut buf = [0; MSS];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap().unwrap();

        assert_eq!(packet, received_packet);
    }
}
