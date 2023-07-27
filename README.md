# Protocol

This is documentation for a reliable protocol over UDP.

<!--  TODO: Should I have a checksum? -->

## Description

Data transfering is done using
[Selective Repeat ARQ](https://en.wikipedia.org/wiki/Selective_Repeat_ARQ).

That uses insights from an article
["An Efficient Selective-Repeat ARQ Scheme for Satellite Channels and Its Throughput Analysis"](https://ieeexplore.ieee.org/document/1094999)

P. Yu and Shu Lin, "An Efficient Selective-Repeat ARQ Scheme for Satellite
Channels and Its Throughput Analysis," in IEEE Transactions on Communications,
vol. 29, no. 3, pp. 353-363, March 1981, doi: 10.1109/TCOM.1981.1094999.

## Discriminants

|     Type      | Discriminant |
| :-----------: | :----------: |
|     INIT      |     0000     |
|     DATA      |     0001     |
|    DATA_OK    |     0101     |
|      NAK      |     0110     |
|  KEEP_ALIVE   |     0010     |
|    INIT_OK    |     0011     |
| KEEP_ALIVE_OK |     0100     |
|      SYN      |     0111     |
|    SYN_ACK    |     1000     |
|  SYN_ACK_ACK  |     1011     |
|      FIN      |     1001     |
|    FIN_OK     |     1010     |

## Layout

### INIT

28 bytes:

- d: 4 bits for discriminant
- p: 11 bits for payload size (valid range [1-1500])
- s: 32 bits for transfer size in bytes
- n: 22 bytes for name (s in little endian)

| 0123456789abcdef | 0123456789abcdef |
| ---------------- | ---------------- |
| ddddppppppppppp0 | ssssssssssssssss |
| ssssssssssssssss | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |

### DATA

4 bytes + payload:

- d: 4 bits for discriminant
- s: 28 bits for sequence number

| 0123456789abcdef |
| ---------------- |
| ddddssssssssssss |
| ssssssssssssssss |
| ......data...... |

### DATA_OK, NAK, SYN_ACK, SYN_ACK_ACK

4 bytes:

- d: 4 bits for discriminant
- s: 28 bits for sequence number

| 0123456789abcdef |
| ---------------- |
| ddddssssssssssss |
| ssssssssssssssss |

### KEEP_ALIVE, INIT_OK, KEEP_ALIVE_OK, FIN, FIN_OK

1 byte:

- d: 4 bits for discriminant

| 0123456789abcdef |
| ---------------- |
| dddd0000........ |
