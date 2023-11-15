# Protocol

This is documentation for a reliable protocol over UDP.

## Description

Data transferring is done using
[Selective Repeat ARQ](https://en.wikipedia.org/wiki/Selective_Repeat_ARQ).

That uses insights from an article
["An Efficient Selective-Repeat ARQ Scheme for Satellite Channels and Its Throughput Analysis"](https://ieeexplore.ieee.org/document/1094999)

P. Yu and Shu Lin, "An Efficient Selective-Repeat ARQ Scheme for Satellite
Channels and Its Throughput Analysis," in IEEE Transactions on Communications,
vol. 29, no. 3, pp. 353-363, March 1981, doi: 10.1109/TCOM.1981.1094999.

Protocol uses modification of Selective-Repeat ARQ, which is based on an
observation that it could degrade to Stop-and-wait ARQ if the first packet of
the window would not be acknowledged but all others would. It could be
beneficial for satellites because it requires less implementation logic, which
implies less probability of a bug and lower cost of the hardware. In that
protocol those benefits were sacrificed if favor of speed. That protocol does
not wait for the leftest packet of the window to be acknowledged, so here window
is not a contiguous sequence of packets, but rather how many unacknowledged
packets could be at the same time.

## Transferring capabilities

Protocol supports transferring of files and text messages up to 4GiB. Filename
could have size from 0 to 22 bytes, encoded in UTF-8.

## Performance

On the loopback interface without checksums protocol gives up to 5.8 Gib/s. With
checksums enabled performance drops to 2.3 Gib/s.

## Sequence diagram

title Handshake

Host A -> Host B: SYN. Host A waits for SYN_ACK for `ACK_TIMEOUT` and resends
SYN if elapsed Host A <- Host B: SYN_ACK. Host B waits for SYN_ACK_ACK for
`ACK_TIMEOUT` and resends SYN_ACK_ACK if elapsed Host A -> Host B: SYN_ACK_ACK.
Host A will resent SYN_ACK_ACK if host B will send SYN_ACK.

title Data transfer

Host C -> Host D: INIT Host C <- Host D: INIT_OK

Host C -> Host D: DATA Host C -> Host D: DATA Host C <- Host D: DATA_OK Host C
-> Host D: DATA Host C <- Host D: DATA_OK Host C <- Host D: DATA_OK

End of transfer is overly complicated for no real reason, just to better
understand how similar mechanism works in TCP. This is not close of the
connection, just to end the transfer.

title End of transfer

Host C -> Host D: FIN. Host C waits for FIN_OK for `10 * ACK_TIMEOUT` and
resends FIN if elapsed Host C <- Host D: FIN_OK. Host D wait for FIN_OK_OK for
`ACK_TIMEOUT` and resends FIN Host C -> Host D: FIN_OK_OK. Host C waits for
`10 * ACK_TIMEOUT` and then stops data transfer

title Keep Alives

Host A -> Host B: KEEP_ALIVE. If host B will not respond in `KA_TIMEOUT` then
connection will be reset. Host A <- Host B: KEEP_ALIVE_OK

## Discriminants

|     Type      | Discriminant |
| :-----------: | :----------: |
|      SYN      |    0b0111    |
|    SYN_ACK    |    0b1000    |
|  SYN_ACK_ACK  |    0b1011    |
|     INIT      |    0b0000    |
|     DATA      |    0b0001    |
|    DATA_OK    |    0b0101    |
|      NAK      |    0b0110    |
|  KEEP_ALIVE   |    0b0010    |
|    INIT_OK    |    0b0011    |
| KEEP_ALIVE_OK |    0b0100    |
|      FIN      |    0b1001    |
|    FIN_OK     |    0b1010    |
|   FIN_OK_OK   |    0b1100    |

## Layout

### INIT

- c: 4 bytes for checksum
- d: 4 bits for discriminant
- p: 11 bits for payload size (valid range [1-1500])
- s: 32 bits for transfer size in bytes
- n: 22 bytes for name (s in little endian)

| 0123456789abcdef | 0123456789abcdef |
| ---------------- | ---------------- |
| cccccccccccccccc | cccccccccccccccc |
| ddddppppppppppp0 | ssssssssssssssss |
| ssssssssssssssss | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |
| nnnnnnnnnnnnnnnn | nnnnnnnnnnnnnnnn |

### DATA

- c: 4 bytes for checksum
- d: 4 bits for discriminant
- s: 28 bits for sequence number

| 0123456789abcdef |
| ---------------- |
| cccccccccccccccc |
| cccccccccccccccc |
| ddddssssssssssss |
| ssssssssssssssss |
| ......data...... |

### DATA_OK, NAK, SYN_ACK, SYN_ACK_ACK

- c: 4 bytes for checksum
- d: 4 bits for discriminant
- s: 28 bits for sequence number

| 0123456789abcdef |
| ---------------- |
| cccccccccccccccc |
| cccccccccccccccc |
| ddddssssssssssss |
| ssssssssssssssss |

### KEEP_ALIVE, INIT_OK, KEEP_ALIVE_OK, FIN, FIN_OK

- c: 4 bytes for checksum
- d: 4 bits for discriminant

| 0123456789abcdef |
| ---------------- |
| cccccccccccccccc |
| cccccccccccccccc |
| dddd0000........ |
