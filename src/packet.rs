#![allow(dead_code)]
const MTU: usize = 1500;

fn main() {
    println!("Hello, World!");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeqNum(pub u32);

#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    Syn,
    SynAck(SeqNum),
    Init {
        payload_size: u16,
        transfer_size: u32,
        name: String,
    },
    Data {
        seq_num: SeqNum,
        //  TODO: Cow (for sender borrow for receiver own)
        data: Vec<u8>,
    },
    Ack(SeqNum),
    Nak(SeqNum),
    KeepAlive,
    InitOk,
    KeepAliveOk,
}

const DISCRIMINANT_MASK: u8 = 0xF0;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum PacketType {
    Syn = 0b0111 << 4,
    SynAck = 0b1000 << 4,
    Init = 0b0000 << 4,
    Data = 0b0001 << 4,
    Ack = 0b0101 << 4,
    Nak = 0b0110 << 4,
    KeepAlive = 0b0010 << 4,
    InitOk = 0b0011 << 4,
    KeepAliveOk = 0b100 << 4,
}

impl TryFrom<u8> for PacketType {
    type Error = ();
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value & !DISCRIMINANT_MASK != 0 {
            return Err(());
        }
        let result = match value >> 4 {
            0b0111 => Ok(Self::Syn),
            0b1000 => Ok(Self::SynAck),
            0b0000 => Ok(Self::Init),
            0b0001 => Ok(Self::Data),
            0b0101 => Ok(Self::Ack),
            0b0110 => Ok(Self::Nak),
            0b0010 => Ok(Self::KeepAlive),
            0b0011 => Ok(Self::InitOk),
            0b0100 => Ok(Self::KeepAliveOk),
            _ => Err(()),
        };
        assert!(
            result.map_or(true, |x| x as u8 == value),
            "Conversion of Packet type is wrong"
        );
        result
    }
}

const ASSERT_CONSTANTS: () = assert_constants();

const fn assert_constants() {
    const DISCRIMINANTS: [PacketType; 9] = [
        PacketType::Syn,
        PacketType::SynAck,
        PacketType::Init,
        PacketType::Data,
        PacketType::KeepAlive,
        PacketType::InitOk,
        PacketType::KeepAliveOk,
        PacketType::Ack,
        PacketType::Nak,
    ];
    const LEN: usize = DISCRIMINANTS.len();
    let mut i = 0;

    while i < LEN {
        assert!(
            DISCRIMINANTS[i] as u8 & !DISCRIMINANT_MASK == 0,
            "Discriminant is wrong"
        );
        i += 1;
    }
}

fn string_from_null_terminated(buf: &[u8]) -> String {
    let name_end = buf.iter().take(22).position(|&x| x == 0).unwrap_or(23);
    String::from_utf8_lossy(&buf[..name_end]).into_owned()
}

impl Packet {
    /// Parses 28 bytes u32 like that
    ///
    /// |0123456789abcdef|
    /// |----------------|
    /// |0000ssssssssssss|
    /// |ssssssssssssssss|
    ///
    //  TODO: Maybe little endian is better
    #[inline] //  TODO: try #[inline(always)]
    fn decode_sequence(bytes: &[u8]) -> SeqNum {
        let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let mask = (DISCRIMINANT_MASK as u32) << 24;
        SeqNum(u32::from_be_bytes(bytes) & !mask)
    }

    #[inline] //  TODO: try #[inline(always)]
    fn encode_sequence(sequence: SeqNum, buf: &mut [u8]) {
        buf.copy_from_slice(&sequence.0.to_be_bytes());
    }

    /// # Panics
    /// When discriminant is unknown.  TODO: Change to return an Option
    #[must_use]
    pub fn deserialize(bytes: &[u8]) -> Option<Self> {
        let discriminant = bytes[0] & DISCRIMINANT_MASK;
        let packet = match PacketType::try_from(discriminant).ok()? {
            PacketType::Init => {
                let first = bytes[0] & !DISCRIMINANT_MASK;
                let payload_size = ((first as u16) << 7) | ((bytes[1] as u16) >> 1);
                let transfer_size = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Self::Init {
                    payload_size,
                    transfer_size,
                    name: string_from_null_terminated(&bytes[6..28]),
                }
            }
            PacketType::Data => Self::Data {
                seq_num: Self::decode_sequence(&bytes[0..4]),
                data: bytes[4..].to_vec(),
            },
            PacketType::Ack => Self::Ack(Self::decode_sequence(&bytes[0..4])),
            PacketType::Nak => Self::Nak(Self::decode_sequence(&bytes[0..4])),
            PacketType::KeepAlive => Self::KeepAlive,
            PacketType::InitOk => Self::InitOk,
            PacketType::KeepAliveOk => Self::KeepAliveOk,
            PacketType::Syn => Self::Syn,
            PacketType::SynAck => Self::SynAck(Self::decode_sequence(&bytes[0..4])),
        };
        Some(packet)
    }

    pub fn serialize(&self, buf: &mut [u8]) -> usize {
        match self {
            Self::Init {
                payload_size,
                transfer_size: _,
                name,
            } => {
                let ps = payload_size & 0b1111_1000_0000_0000 == 0;
                let len = name.bytes().len() <= 22;
                debug_assert!(ps, "Payload size should have 5 high bits zero");
                debug_assert!(len, "Name should not be greater then 22 bytes");
            }
            Self::Data { seq_num, data: _ }
            | Self::Ack(seq_num)
            | Self::Nak(seq_num)
            | Self::SynAck(seq_num) => {
                let seq = seq_num.0 & 0xF0_00_00_00 == 0;
                debug_assert!(seq, "Seq num should have 4 high bits zero");
            }

            Self::Syn | Self::KeepAlive | Self::InitOk | Self::KeepAliveOk => {}
        }

        let result_len;

        match self {
            Self::Init {
                payload_size,
                transfer_size,
                name,
            } => {
                let payload_size = *payload_size;
                buf[0] = (payload_size >> 7) as u8;
                buf[1] = ((payload_size as u8) & 0b0111_1111) << 1;
                buf[2..6].copy_from_slice(&transfer_size.to_le_bytes());
                buf[6..][..name.len()].copy_from_slice(name.as_bytes());

                result_len = 28;
            }
            Self::Data { seq_num, data } => {
                Self::encode_sequence(*seq_num, &mut buf[0..4]);
                buf[4..][..data.len()].copy_from_slice(data);
                result_len = 4 + data.len();
            }
            Self::SynAck(seq_num) | Self::Ack(seq_num) | Self::Nak(seq_num) => {
                Self::encode_sequence(*seq_num, &mut buf[0..4]);
                result_len = 4;
            }
            Self::Syn | Self::KeepAlive | Self::InitOk | Self::KeepAliveOk => result_len = 1,
        }

        let discriminant = match self {
            Self::Init { .. } => PacketType::Init,
            Self::Data { .. } => PacketType::Data,
            Self::Ack(..) => PacketType::Ack,
            Self::Nak(..) => PacketType::Nak,
            Self::KeepAlive => PacketType::KeepAlive,
            Self::InitOk => PacketType::InitOk,
            Self::KeepAliveOk => PacketType::KeepAliveOk,
            Self::Syn => PacketType::Syn,
            Self::SynAck(..) => PacketType::SynAck,
        };

        buf[0] &= !DISCRIMINANT_MASK;
        buf[0] |= discriminant as u8;
        // maybe `buf[0] ^= discriminant`, but idk how it works

        result_len
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
        if let Packet::Init {
            payload_size,
            transfer_size,
            name,
        } = packet
        {
            assert_eq!(payload_size, 0b100_0100_0000);
            assert_eq!(name, "hello");
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
        if let Packet::Data {
            seq_num: sequence,
            data,
        } = packet
        {
            assert_eq!(sequence.0, 0b0000_1000_0000_1111_1111);
            assert_eq!(data, b"data!");
        } else {
            panic!("Unexpected packet type");
        }
    }

    #[test]
    fn test_init_packet() {
        let packet = Packet::Init {
            payload_size: 512,
            transfer_size: 10000,
            name: String::from("testfile"),
        };

        let mut buf = vec![0; 28];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_data_packet() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = vec![0; 4 + data.len()];

        let packet = Packet::Data {
            seq_num: SeqNum(5),
            data,
        };

        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ack_packet() {
        let packet = Packet::Ack(SeqNum(10));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_nack_packet() {
        let packet = Packet::Nak(SeqNum(20));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_keep_alive_packet() {
        let packet = Packet::KeepAlive;

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_init_ok_packet() {
        let packet = Packet::InitOk;

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ka_ok_packet() {
        let packet = Packet::KeepAliveOk;

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf).unwrap();

        assert_eq!(packet, received_packet);
    }
}
