use std::{ops::AddAssign, str::Utf8Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeqNum(pub u32);

impl AddAssign for SeqNum {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.wrapping_add(rhs.0);
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
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
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
}

/// Packet that Receiver sends to Sender
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RecvPacket {
    DataAck(SeqNum),
    Nak(SeqNum),
    InitOk,
    FinOk,
}

const DISCRIMINANT_MASK: u8 = 0xF0;

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
    KeepAliveOk = 0b100 << 4,
    Fin = 0b1001 << 4,
    FinOk = 0b1010 << 4,
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
    if value & !DISCRIMINANT_MASK != 0 {
        return Err(());
    }
    let result = match value >> 4 {
        0b0111 => Ok(PacketType::Syn),
        0b1000 => Ok(PacketType::SynAck),
        0b1011 => Ok(PacketType::SynAckAck),
        0b0000 => Ok(PacketType::Init),
        0b0001 => Ok(PacketType::Data),
        0b0101 => Ok(PacketType::DataOk),
        0b0110 => Ok(PacketType::Nak),
        0b0010 => Ok(PacketType::KeepAlive),
        0b0011 => Ok(PacketType::InitOk),
        0b0100 => Ok(PacketType::KeepAliveOk),
        0b1001 => Ok(PacketType::Fin),
        0b1010 => Ok(PacketType::FinOk),
        _ => Err(()),
    };
    assert!(
        match result {
            Ok(x) => x as u8 == value,
            Err(_) => true,
        },
        "Conversion of Packet type is wrong"
    );
    result
}

#[inline]
fn string_from_null_terminated(buf: &[u8]) -> Result<Option<&str>, Utf8Error> {
    let name_end = buf.iter().take(22).position(|&x| x == 0).unwrap_or(23);
    if name_end == 0 {
        return Ok(None);
    }
    std::str::from_utf8(&buf[..name_end]).map(Some)
}

impl<'a> SendPacket<'a> {
    #[inline]
    pub fn serialize(&self, buf: &mut [u8]) -> usize {
        Packet::Send(*self).serialize(buf)
    }
}

impl RecvPacket {
    #[inline]
    pub fn serialize(self, buf: &mut [u8]) -> usize {
        Packet::Recv(self).serialize(buf)
    }
}

impl ConnPacket {
    #[inline]
    pub fn serialize(self, buf: &mut [u8]) -> usize {
        Packet::Conn(self).serialize(buf)
    }
}

impl<'a> Packet<'a> {
    /// Parses 28 bytes u32 like that
    ///
    /// |0123456789abcdef|
    /// |----------------|
    /// |0000ssssssssssss|
    /// |ssssssssssssssss|
    ///
    #[inline]
    fn decode_sequence(bytes: &[u8]) -> SeqNum {
        assert!(
            bytes.len() == 4,
            "Sequece number could be decoded only from 4 bytes"
        );
        let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let mask = (DISCRIMINANT_MASK as u32) << 24;
        SeqNum(u32::from_be_bytes(bytes) & !mask)
    }

    #[inline]
    fn encode_sequence(sequence: SeqNum, buf: &mut [u8]) {
        buf.copy_from_slice(&sequence.0.to_be_bytes());
    }

    #[must_use]
    #[inline]
    pub fn deserialize(bytes: &'a [u8]) -> Option<Self> {
        let discriminant = bytes[0] & DISCRIMINANT_MASK;
        let packet = match PacketType::try_from(discriminant).ok()? {
            PacketType::Init => {
                let first = bytes[0] & !DISCRIMINANT_MASK;
                let payload_size = ((first as u16) << 7) | ((bytes[1] as u16) >> 1);
                let transfer_size = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Self::Send(SendPacket::Init {
                    payload_size,
                    transfer: transfer_size,
                    name: string_from_null_terminated(&bytes[6..28]).ok()?,
                })
            }
            PacketType::Data => Self::Send(SendPacket::Data {
                seq_num: Self::decode_sequence(&bytes[0..4]),
                data: &bytes[4..],
            }),
            PacketType::DataOk => {
                Self::Recv(RecvPacket::DataAck(Self::decode_sequence(&bytes[0..4])))
            }
            PacketType::Nak => Self::Recv(RecvPacket::Nak(Self::decode_sequence(&bytes[0..4]))),
            PacketType::KeepAlive => Self::Conn(ConnPacket::KeepAlive),
            PacketType::InitOk => Self::Recv(RecvPacket::InitOk),
            PacketType::KeepAliveOk => Self::Conn(ConnPacket::KeepAliveOk),
            PacketType::Syn => Self::Conn(ConnPacket::Syn),
            PacketType::SynAck => {
                Self::Conn(ConnPacket::SynAck(Self::decode_sequence(&bytes[0..4])))
            }
            PacketType::SynAckAck => {
                Self::Conn(ConnPacket::SynAckAck(Self::decode_sequence(&bytes[0..4])))
            }
            PacketType::Fin => Self::Send(SendPacket::Fin),
            PacketType::FinOk => Self::Recv(RecvPacket::FinOk),
        };
        Some(packet)
    }

    #[inline]
    pub fn serialize(&self, buf: &mut [u8]) -> usize {
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
                let seq = seq_num.0 & 0xF0_00_00_00 == 0;
                debug_assert!(seq, "Seq num should have 4 high bits zero");
            }

            Self::Send(SendPacket::Fin) | Self::Conn(_) | Self::Recv(_) => {}
        }
        let result_len;

        match *self {
            Self::Send(SendPacket::Init {
                payload_size,
                transfer: transfer_size,
                name,
            }) => {
                let payload_size = payload_size;
                buf[0] = (payload_size >> 7) as u8;
                buf[1] = ((payload_size as u8) & 0b0111_1111) << 1;
                buf[2..6].copy_from_slice(&transfer_size.to_le_bytes());
                if let Some(name) = name {
                    buf[6..][..name.len()].copy_from_slice(name.as_bytes());
                }

                result_len = 28;
            }
            Self::Send(SendPacket::Data { seq_num, data }) => {
                Self::encode_sequence(seq_num, &mut buf[0..4]);
                buf[4..][..data.len()].copy_from_slice(data);
                result_len = 4 + data.len();
            }
            Self::Conn(ConnPacket::SynAckAck(seq_num) | ConnPacket::SynAck(seq_num))
            | Self::Recv(RecvPacket::DataAck(seq_num) | RecvPacket::Nak(seq_num)) => {
                Self::encode_sequence(seq_num, &mut buf[0..4]);
                result_len = 4;
            }
            Self::Send(SendPacket::Fin)
            | Self::Recv(RecvPacket::FinOk | RecvPacket::InitOk)
            | Self::Conn(ConnPacket::Syn | ConnPacket::KeepAlive | ConnPacket::KeepAliveOk) => {
                result_len = 1;
            }
        }

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
        };

        buf[0] &= !DISCRIMINANT_MASK;
        buf[0] |= discriminant as u8;
        // maybe `buf[0] ^= discriminant`, but idk how it works

        result_len
    }
}

#[allow(dead_code)]
const ASSERT_CONSTANTS: () = assert_constants();

const fn assert_constants() {
    const DISCRIMINANTS: [PacketType; 12] = [
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
    ];
    const LEN: usize = DISCRIMINANTS.len();
    let mut i = 0;

    while i < LEN {
        assert!(
            match try_from(DISCRIMINANTS[i] as u8) {
                Ok(d) => d as u8 == DISCRIMINANTS[i] as u8,
                Err(_) => false,
            },
            "Try from should work fine."
        );
        assert!(
            DISCRIMINANTS[i] as u8 & !DISCRIMINANT_MASK == 0,
            "Discriminant is wrong"
        );
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MTU: usize = 1500;

    #[test]
    fn encodings_decodings() {
        let check = |n| {
            let mut buf = [0; 4];
            Packet::encode_sequence(SeqNum(n), &mut buf);
            assert_eq!(buf[0] & DISCRIMINANT_MASK, 0, "Test malformed");
            assert_eq!(SeqNum(n), Packet::decode_sequence(&buf));
        };
        check(0x0F_FF_FF_0F);
        check(0x0A_BB_CC_0F);
        check(0x01_23_45_0F);
    }

    #[test]
    fn test_parse_init() {
        let mut bytes = [0u8; MTU];
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
        let packet = Packet::deserialize(&bytes).unwrap();
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
        let packet = Packet::deserialize(&bytes).unwrap();
        if let Packet::Send(SendPacket::Data {
            seq_num: sequence,
            data,
        }) = packet
        {
            assert_eq!(sequence.0, 0b0000_1000_0000_1111_1111);
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

        let mut buf = vec![0; 28];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_data_packet() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = vec![0; 4 + data.len()];

        let packet = SendPacket::Data {
            seq_num: SeqNum(5),
            data: &data,
        };

        packet.serialize(&mut buf);
        let Packet::Send(received_packet) = Packet::deserialize(&buf).unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ack_packet() {
        let packet = RecvPacket::DataAck(SeqNum(10));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let Packet::Recv(received_packet) = Packet::deserialize(&buf).unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_nack_packet() {
        let packet = RecvPacket::Nak(SeqNum(20));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let Packet::Recv(received_packet) = Packet::deserialize(&buf).unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }
    #[test]
    fn test_syn_ack_ack_packet() {
        let packet = ConnPacket::SynAckAck(SeqNum(10));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let Packet::Conn(received_packet) = Packet::deserialize(&buf).unwrap() else {
            panic!("Unexpected packet type");
        };

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_keep_alive_packet() {
        let packet = Packet::Conn(ConnPacket::KeepAlive);

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_init_ok_packet() {
        let packet = Packet::Recv(RecvPacket::InitOk);

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ka_ok_packet() {
        let packet = Packet::Conn(ConnPacket::KeepAliveOk);

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }
}
