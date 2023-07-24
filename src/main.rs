use protocol::{Message, Server};
use tokio::time::Instant;

#[tokio::main]
async fn main() {
    const ADDR1: &str = "127.0.0.1:6201";
    const ADDR2: &str = "127.0.0.1:6202";

    let size = 400 * 2usize.pow(20);
    let msg = Message::file(vec![5u8; size], 1496, "Ivakura.txt".to_string());
    let msg_clone = msg.clone();
    let start = Instant::now();

    let h1 = tokio::spawn(async move {
        let server = Server::bind(ADDR1).await.unwrap();
        let mut connection = server.connect(ADDR2).await.unwrap();
        let (mut tx, _) = connection.split();
        tx.send(msg_clone).await
    });
    let h2 = tokio::spawn(async move {
        let server = Server::bind(ADDR2).await.unwrap();
        let mut connection = server.listen().await.unwrap();
        let (_, mut rx) = connection.split();
        rx.recv().await
    });

    let (r1, r2) = tokio::join!(h1, h2);
    let end = Instant::now();
    r1.unwrap().unwrap();
    let transfered_message = r2.unwrap().unwrap();
    assert_eq!(transfered_message, msg, "Message is corrupted");
    println!(
        "Speed: {}Mib/sec",
        (size / 1000 * 8) / (end - start).as_millis() as usize
    );
}
