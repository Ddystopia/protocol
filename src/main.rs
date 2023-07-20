#![allow(dead_code, unused_imports)]
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time;

use protocol::packet::Packet;

const A_PORT: &str = "127.0.0.1:3000";
const B_PORT: &str = "127.0.0.1:4000";

const MTU: usize = 1500;

async fn start(machine: &str) -> Result<(), Box<dyn Error>> {
    let (tx, mut rx) = mpsc::channel(100);

    let (this, other) = match machine.trim() {
        "A" => (A_PORT, B_PORT),
        "B" => (B_PORT, A_PORT),
        _ => panic!("Invalid machine"),
    };

    let socket = Arc::new(UdpSocket::bind(this.to_string()).await?);

    socket.connect(other.to_string()).await?;
    println!("Connected to {}", other);

    // Reads stdin to send messages to the other peer
    tokio::spawn({
        let socket = socket.clone();
        async move {
            let mut buf = [0; MTU];
            loop {
                let _len = socket.recv_from(&mut buf).await.expect("Failed to recv");
                let packet = Packet::deserialize(&buf);
                let _ = tx.send(packet).await;
            }
        }
    });

    if this == A_PORT {
        let mut buf = [0; MTU];
        let packet = Packet::Init {
            payload_size: 512,
            transfer_size: 10000,
            name: String::from("testfile"),
        };

        packet.serialize(&mut buf);

        let _ = socket.send(&buf).await;
    }

    while let Some(packet) = rx.recv().await {
        println!("Recv: {packet:?}");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let delay_for_active = tokio::time::sleep(Duration::from_millis(100));

    let hs1 = tokio::spawn(async move {
        delay_for_active.await;
        start("A").await.ok()
    });

    let hs2 = tokio::spawn(async move { start("B").await.ok() });

    let (r1, r2) = tokio::join!(hs1, hs2);
    r1.unwrap().unwrap();
    r2.unwrap().unwrap();
    Ok(())
}
