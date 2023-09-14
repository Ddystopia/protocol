#![allow(dead_code)]
#![allow(unused_imports)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::{sync::oneshot, net::UdpSocket};
use std::time::Duration;
#[cfg(feature = "mock")]
use std::array::from_fn;


/*
(loss is should be doubled of what is written)

Loss:      0.00%      0.10%      0.50%      1.00%      3.00%      5.00%      7.00%
Try 0      4760       4665       4662       4575       2043       1219        828
Try 1      4723       4711       4650       4619       2049       1191        806
Try 2      4725       4700       4698       4645       1998       1175        829
Try 3      4756       4690       4635       4618       2037       1133        782
Try 4      4720       4667       4666       4599       2047       1216        835
Try 5      4712       4729       4696       4538       2007       1226        826
Avgs:      4732       4693       4667       4599       2030       1193        817
Reg :      0.00%      0.82%      1.37%      2.81%     57.10%     74.79%     82.73%
*/

#[cfg(not(feature = "mock"))]
#[tokio::main]
async fn main() {
    let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0));
    let speed = test(0, 1, SocketAddr::new(ip, 10200), SocketAddr::new(ip, 10201)).await;
    // let speed = test_udp(SocketAddr::new(ip, 10200), SocketAddr::new(ip, 10201)).await;
    println!("Speed: {}Mib/sec", speed);
}

#[cfg(feature = "mock")]
#[tokio::main]
async fn main() {
    const DEN: u32 = 10000;
    const TABLE: [u32; 5] = [100, 150, 200, 250, 300];
    // const TABLE: [u32; 10] = [0, 10, 50, 100, 300, 500, 700, 1000, 2500, 3500];
    const TRYES: usize = 6;
    let mut results: [[usize; TABLE.len()]; TRYES] = from_fn(|_| from_fn(|_| 0));
    let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0));
    for (i, num) in TABLE.into_iter().enumerate() {
        for (j, result_row) in results.iter_mut().enumerate() {
            println!("\n{i}/{j} <- Start");
            let count = i * 10 + j;
            result_row[i] = test(
                num,
                DEN,
                SocketAddr::new(ip, (10200 + 2 * count) as u16),
                SocketAddr::new(ip, (10200 + 2 * count + 1) as u16),
            )
            .await;
            println!("{i}/{j} <- End");
        }
    }
    let avg_results: [usize; TABLE.len()] =
        from_fn(|i| results.iter().fold(0, |s, r| s + r[i]) / results.len());

    println!("Results:");
    print!("Loss:");
    for n in TABLE {
        print!("{:10.2}%", n as f64 / DEN as f64 * 100.0);
    }
    println!();
    for (i, try_row) in results.iter().enumerate() {
        print!("Try {:1}", i);
        for n in try_row {
            print!("{:10} ", n);
        }
        println!();
    }
    print!("Avgs:");
    for n in avg_results {
        print!("{:10} ", n);
    }
    println!();
    print!("Reg1:");
    for n in avg_results {
        print!("{:10.2}%", 100. - n as f64 / avg_results[0] as f64 * 100.);
    }
    println!();
    print!("Reg2:");
    for n in avg_results {
        print!(
            "{:10.2}%",
            100. / (1. - n as f64 / avg_results[0] as f64 * 1.)
        );
    }
    println!();
}

// #[cfg(feature = "mock")]
async fn test(n: u32, d: u32, a1: SocketAddr, a2: SocketAddr) -> usize {
    use protocol::{Message, Server, MAX_TRANSFER_SIZE};
    use tokio::time::Instant;

    #[cfg(feature = "mock")]
    protocol::mock::set_num_den(n, d);
    #[cfg(not(feature = "mock"))]
    std::hint::black_box((n, d));
    // return 300;
    let size = 4000 * 2usize.pow(20);
    let msg = Message::file(
        vec![5u8; size],
        MAX_TRANSFER_SIZE,
        "Ivakura.txt".to_string(),
    );
    let msg_clone = msg.clone();
    let (start_tx, start_rx) = oneshot::channel();

    let h1 = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        start_tx.send(Instant::now()).unwrap();
        let server = Server::bind(a1).await.expect("Server 1");
        let mut connection = server.connect(a2).await.expect("Conn 1");
        let (mut tx, _) = connection.split();
        let r = tx.send(msg_clone).await;
        println!("Active Sent");
        connection.disconnect().await.expect("Disconnect 1");
        println!("Active Disconnected");
        r
    });

    let h2 = tokio::spawn(async move {
        let server = Server::bind(a2).await.expect("Server 2");
        let mut connection = server.listen().await.expect("Conn 2");
        let (_, mut rx) = connection.split();
        let r = rx.recv().await;
        println!("Passive Done");
        let duration = start_rx.await.unwrap().elapsed();
        connection.disconnect().await.expect("Disconnect 2");
        println!("Passive Disconnected");
        (r, duration)
    });

    let (r1, r2) = tokio::join!(h1, h2);
    r1.expect("Join").expect("Send Error");
    let (transfered_message, duration) = r2.expect("Join");
    let transfered_message = transfered_message.expect("No message");
    assert_eq!(transfered_message, msg, "Message is corrupted");
    (size / 1000 * 8) / duration.as_millis() as usize
}



async fn test_udp(a1: SocketAddr, a2: SocketAddr) -> usize {
    use protocol::{Message, Server, MAX_TRANSFER_SIZE};
    use tokio::time::Instant;

    let size = 4000 * 2usize.pow(20);
    let msg = vec![5u8; size];
    // let msg_clone = msg.clone();
    let (start_tx, start_rx) = oneshot::channel();
    let (end_tx, mut end_rx) = oneshot::channel();


    let h1 = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        start_tx.send(Instant::now()).unwrap();
        let socket = UdpSocket::bind(a1).await.expect("Server 1");
        socket.connect(a2).await.expect("Connect 1");
        println!("Active Start Sending");
        for part in msg.chunks(MAX_TRANSFER_SIZE.into()) {
            socket.send(part).await.expect("Send 1");
        }
        println!("Active Sent");
        end_tx.send(()).unwrap();
    });

    let h2 = tokio::spawn(async move {
        let socket = UdpSocket::bind(a2).await.expect("Server 2");
        while end_rx.try_recv().is_err() {
            let mut buf = vec![0u8; MAX_TRANSFER_SIZE as usize];
            let v = socket.recv_from(&mut buf).await.expect("Recv 2");
            std::hint::black_box(v);
        }
        start_rx.await.unwrap().elapsed()
    });

    let (r1, r2) = tokio::join!(h1, h2);
    r1.expect("Join");
    let duration = r2.expect("Join");
    (size / 1000 * 8) / duration.as_millis() as usize
}
