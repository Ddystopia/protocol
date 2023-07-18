#![allow(dead_code)]
const MTU: usize = 1500;

fn main() {
    println!("Hello, World!");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeqNum(pub u32);

#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
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
    KaOk,
}

const DISCRIMINANT_MASK: u8 = 0xF0;

const INIT: u8 = 0b0000 << 4;
const DATA: u8 = 0b0001 << 4;
const KEEP_ALIVE: u8 = 0b0010 << 4;
const INIT_OK: u8 = 0b0011 << 4;
const KEEP_ALIVE_OK: u8 = 0b100 << 4;
const ACK: u8 = 0b0101 << 4;
const NAK: u8 = 0b0110 << 4;

const ASSERT_CONSTANTS: () = assert_constants();

const fn assert_constants() {
    const DISCRIMINANTS: [u8; 7] = [INIT, DATA, KEEP_ALIVE, INIT_OK, KEEP_ALIVE_OK, ACK, NAK];
    let mut i = 0;

    while i < 7 {
        assert!(
            DISCRIMINANTS[i] & !DISCRIMINANT_MASK == 0,
            "Discriminant is wrong"
        );
        i += 1;
    }
}

fn string_from_null_terminated(buf: &[u8]) -> String {
    let name_end = buf.iter().take(23).position(|&x| x == 0).unwrap_or(23);
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
    pub fn deserialize(bytes: &[u8]) -> Self {
        let discriminant = bytes[0] & DISCRIMINANT_MASK;
        match discriminant {
            INIT => {
                let first = bytes[0] & !DISCRIMINANT_MASK;
                let payload_size = ((first as u16) << 7) | ((bytes[1] as u16) >> 1);
                let transfer_size = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Self::Init {
                    payload_size,
                    transfer_size,
                    name: string_from_null_terminated(&bytes[6..28]),
                }
            }
            DATA => Self::Data {
                seq_num: Self::decode_sequence(&bytes[0..4]),
                data: bytes[4..].to_vec(),
            },
            ACK => Self::Ack(Self::decode_sequence(&bytes[0..4])),
            NAK => Self::Nak(Self::decode_sequence(&bytes[0..4])),
            KEEP_ALIVE => Self::KeepAlive,
            INIT_OK => Self::InitOk,
            KEEP_ALIVE_OK => Self::KaOk,
            _ => panic!("Unknown packet discriminant"),
        }
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
            Self::Data { seq_num, data: _ } | Self::Ack(seq_num) | Self::Nak(seq_num) => {
                let seq = seq_num.0 & 0xF0_00_00_00 == 0;
                debug_assert!(seq, "Seq num should have 4 high bits zero");
            }

            Self::KeepAlive | Self::InitOk | Self::KaOk => {}
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
            Self::Ack(seq_num) | Self::Nak(seq_num) => {
                Self::encode_sequence(*seq_num, &mut buf[0..4]);
                result_len = 4;
            }
            Self::KeepAlive | Self::InitOk | Self::KaOk => result_len = 1
        }

        let discriminant = match self {
            Self::Init { .. } => INIT,
            Self::Data { .. } => DATA,
            Self::Ack(..) => ACK,
            Self::Nak(..) => NAK,
            Self::KeepAlive => KEEP_ALIVE,
            Self::InitOk => INIT_OK,
            Self::KaOk => KEEP_ALIVE_OK,
        };

        buf[0] &= !DISCRIMINANT_MASK;
        buf[0] |= discriminant;
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
        let packet = Packet::deserialize(&bytes);
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
        let packet = Packet::deserialize(&bytes);
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
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_data_packet() {
        let data = vec![1, 2, 3, 4, 5];
        let mut buf = vec![0; 4 + data.len()];

        let packet = Packet::Data { seq_num: SeqNum(5), data };

        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ack_packet() {
        let packet = Packet::Ack(SeqNum(10));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_nack_packet() {
        let packet = Packet::Nak(SeqNum(20));

        let mut buf = [0; 4];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_keep_alive_packet() {
        let packet = Packet::KeepAlive;

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_init_ok_packet() {
        let packet = Packet::InitOk;

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }

    #[test]
    fn test_ka_ok_packet() {
        let packet = Packet::KaOk;

        let mut buf = [0; 1];
        packet.serialize(&mut buf);
        let received_packet = Packet::deserialize(&buf);

        assert_eq!(packet, received_packet);
    }
}
