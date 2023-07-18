use std::error::Error;
use std::io::Write;
use std::sync::Arc;
use tokio::io::{stdin, AsyncBufReadExt, BufReader};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use protocol::packet::Packet;

const A_PORT: &str = "127.0.0.1:3000";
const B_PORT: &str = "127.0.0.1:4000";

const MTU: usize = 1500;

fn usage1() {
    let this = "127.0.0.1:3000";
    let other = "127.0.0.1:4000";

    let server = Server::bind(this);
    let connection = server.connect(other);

    {
        let (sender, receiver) = connection.split(); // mut borrow of connection

        // send and recv are taking `&mut self`

        let sending_result = sender.send(Message::File(file)).await;
        let message = receiver.recv().await;
    }

    {
        let (sender, receiver) = connection.split();
    // ...
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (tx, mut rx) = mpsc::channel(100);

    print!("Enter Mathine A or B: ");
    std::io::stdout().flush().unwrap();

    let mut machine = String::new();
    let mut reader = BufReader::new(stdin());
    reader.read_line(&mut machine).await?;
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

