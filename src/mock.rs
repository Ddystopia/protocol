#![allow(clippy::missing_panics_doc)]
use rand::{rngs::StdRng, RngCore, SeedableRng};
pub(crate) use std::net::ToSocketAddrs;
use std::{array::from_fn, io, mem::MaybeUninit, net::SocketAddr, sync::OnceLock};
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::{packet::Packet, MSS};
use std::sync::atomic::Ordering::Relaxed;

const MAX_PORT: usize = 10700;
const MIN_PORT: usize = 10200;
const PORTS: usize = MAX_PORT - MIN_PORT + 1;
type Pack = (usize, [u8; MSS]);
type Sockets = [OnceLock<SocketAddr>; PORTS];
type Reads = [Mutex<mpsc::Receiver<(Pack, SocketAddr)>>; PORTS];
type Writes = [RwLock<mpsc::Sender<(Pack, SocketAddr)>>; PORTS];

#[cfg(feature = "mock")]
pub fn set_num_den(n: u32, d: u32) {
    crate::NUM_LOSS.store(n, Relaxed);
    crate::DEN_LOSS.store(d, Relaxed);
}

async fn get_random_u32() -> u32 {
    static RNG: OnceLock<Mutex<StdRng>> = OnceLock::new();
    RNG.get_or_init(|| Mutex::new(<StdRng as SeedableRng>::from_entropy()))
        .lock()
        .await
        .next_u32()
}

fn sockets() -> &'static Sockets {
    static SOCKETS: OnceLock<Sockets> = OnceLock::new();
    SOCKETS.get_or_init(|| from_fn(|_| OnceLock::new()))
}

fn rw() -> &'static (Reads, Writes) {
    type ReadsUninit = MaybeUninit<[Mutex<mpsc::Receiver<(Pack, SocketAddr)>>; PORTS]>;
    type WritesUninit = MaybeUninit<[RwLock<mpsc::Sender<(Pack, SocketAddr)>>; PORTS]>;
    type RecBlock = Mutex<mpsc::Receiver<(Pack, SocketAddr)>>;
    type SenBlock = RwLock<mpsc::Sender<(Pack, SocketAddr)>>;

    static RW: OnceLock<(Reads, Writes)> = OnceLock::new();
    RW.get_or_init(|| {
        use std::mem::transmute;
        let rw = (0..PORTS).map(|_| mpsc::channel(100_000)).enumerate();
        let mut rs: [MaybeUninit<RecBlock>; PORTS] = unsafe { MaybeUninit::uninit().assume_init() };
        let mut ws: [MaybeUninit<SenBlock>; PORTS] = unsafe { MaybeUninit::uninit().assume_init() };

        for (i, (w, r)) in rw {
            rs[i].write(Mutex::new(r));
            ws[i].write(RwLock::new(w));
        }

        unsafe {
            (
                transmute::<_, ReadsUninit>(rs).assume_init(),
                transmute::<_, WritesUninit>(ws).assume_init(),
            )
        }
    })
}

fn reads() -> &'static Reads {
    &rw().0
}
fn writes() -> &'static Writes {
    &rw().1
}

#[derive(Clone, Debug)]
pub struct Mock(SocketAddr);
impl Mock {
    fn idx(&self) -> usize {
        self.0.port() as usize - MIN_PORT
    }
}

impl Mock {
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self>
    where
        Self: Sized,
    {
        async {}.await;
        let addr = addr.to_socket_addrs()?.next().expect("no addr");
        Ok(Self(addr))
    }
    pub async fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        async {}.await;
        let addr = addr.to_socket_addrs()?.next().expect("no addr");
        println!(
            "Id {} connects to {}",
            self.idx(),
            addr.port() - MIN_PORT as u16
        );
        sockets()[self.idx()].set(addr).expect("Failed to connect");
        Ok(())
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let (len, addr) = Self::recv_from(self, buf).await?;
        assert!(
            Some(&addr) == sockets()[self.idx()].get(),
            "Received from stranger."
        );
        Ok(len)
    }
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        match reads()[self.idx()].lock().await.recv().await {
            Some(((len, bytes), addr)) => {
                buf[..len].copy_from_slice(&bytes[..len]);
                Ok((len, addr))
            }
            None => Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
        }
    }

    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let (n, d) = (crate::NUM_LOSS.load(Relaxed), crate::DEN_LOSS.load(Relaxed));
        if get_random_u32().await < u32::MAX / d * n {
            let packet = Packet::deserialize(buf).expect("Bad packet sent");
            // println!("Dropping {packet:?}");
            return Ok(buf.len());
        }
        // println!("Id {} sends", self.idx());
        self.send_to(
            buf,
            sockets()[self.idx()].get().expect("Send not connected"),
        )
        .await
    }
    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> io::Result<usize> {
        let addr = target.to_socket_addrs()?.next().expect("no addr");
        let mut to_send = (buf.len(), [0; MSS]);
        to_send.1[..buf.len()].copy_from_slice(buf);
        let _ = writes()[addr.port() as usize - MIN_PORT]
            .read()
            .await
            .send((to_send, self.0))
            .await;
        Ok(buf.len())
    }
}
