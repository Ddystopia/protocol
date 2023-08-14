// #![allow(clippy::type_complexity)]

pub(crate) use std::net::ToSocketAddrs;
use std::{array::from_fn, io, mem::MaybeUninit, net::SocketAddr, sync::OnceLock};
use tokio::{sync::mpsc, sync::Mutex};

use crate::{MSS, packet::Packet};

const MAX_PORT: usize = 10210;
const MIN_PORT: usize = 10201;
const PORTS: usize = MAX_PORT - MIN_PORT + 1;
type Pack = (usize, [u8; MSS]);
type Sockets = Mutex<[Option<(SocketAddr, Option<SocketAddr>)>; PORTS]>;
type Reads = Mutex<[mpsc::Receiver<(Pack, SocketAddr)>; PORTS]>;
type Writes = Mutex<[mpsc::Sender<(Pack, SocketAddr)>; PORTS]>;

fn sockets() -> &'static Sockets {
    static SOCKETS: OnceLock<Sockets> = OnceLock::new();
    SOCKETS.get_or_init(|| Mutex::new(from_fn(|_| None)))
}

fn rw() -> &'static (Reads, Writes) {
    type ReadsUninit = MaybeUninit<[mpsc::Receiver<(Pack, SocketAddr)>; PORTS]>;
    type WritesUninit = MaybeUninit<[mpsc::Sender<(Pack, SocketAddr)>; PORTS]>;

    static RW: OnceLock<(Reads, Writes)> = OnceLock::new();
    RW.get_or_init(|| {
        use std::mem::transmute;
        let rw = (0..PORTS).map(|_| mpsc::channel(1000)).enumerate();
        let mut rs: [MaybeUninit<mpsc::Receiver<(Pack, SocketAddr)>>; PORTS] =
            unsafe { MaybeUninit::uninit().assume_init() };
        let mut ws: [MaybeUninit<_>; PORTS] = unsafe { MaybeUninit::uninit().assume_init() };

        for (i, (w, r)) in rw {
            rs[i].write(r);
            ws[i].write(w);
        }

        unsafe {
            (
                Mutex::new(transmute::<_, ReadsUninit>(rs).assume_init()),
                Mutex::new(transmute::<_, WritesUninit>(ws).assume_init()),
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
        let addr = addr.to_socket_addrs()?.next().unwrap();
        let idx = addr.port() as usize - MIN_PORT;
        let mut sockets = sockets().lock().await;
        sockets[idx] = Some((addr, None));
        Ok(Self(sockets[idx].unwrap().0))
    }
    pub async fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let addr = addr.to_socket_addrs()?.next().unwrap();
        let mut sockets = sockets().lock().await;
        sockets[self.idx()].as_mut().unwrap().1 = Some(addr);
        println!("Conn: {:?}", sockets);
        Ok(())
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let (len, addr) = Self::recv_from(self, buf).await?;
        assert!(
            Some(addr) == sockets().lock().await[self.idx()].and_then(|it| it.1),
            "Received from stranger."
        );
        Ok(len)
    }
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        // println!("Here R");
        match reads().lock().await[self.idx()].recv().await {
            Some(((len, bytes), addr)) => {
                buf[..len].copy_from_slice(&bytes[..len]);
                let packet = Packet::deserialize(&buf[..len]);
                println!("Idx {} Recv {packet:?}", self.idx());
                Ok((len, addr))
            }
            None => Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
        }
    }

    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.send_to(
            buf,
            sockets().lock().await[self.idx()]
                .and_then(|it| it.1)
                .unwrap(),
        )
        .await
    }
    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> io::Result<usize> {
        // static C: AtomicUsize = AtomicUsize::new(1);
        // println!(
        //     "Here S {}",
        //     C.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        // );
        let addr = target.to_socket_addrs()?.next().unwrap();
        let mut to_send = (buf.len(), [0; MSS]);
        to_send.1[..buf.len()].copy_from_slice(buf);
        let _ = writes().lock().await[addr.port() as usize - MIN_PORT]
            .send((to_send, self.0))
            .await;
        let packet = Packet::deserialize(buf);
        println!(
            "Idx {} Sent {:?}",
            self.idx(),
            packet,
            // to_send.0,
            // &to_send.1[..to_send.0]
        );
        Ok(buf.len())
    }
}
