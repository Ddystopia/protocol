// use tokio_util::time::DelayQueue;
//
// use packet::SeqNum;
// use packet::Packet;

// const MTU: usize = 1500;
// const WINDOW_SIZE: usize = 128;

use tokio::sync::mpsc;

use crate::Message;

pub async fn event_loop(
    mut _api_sender_notify_tx: mpsc::Sender<()>,
    mut api_sender_rx: mpsc::Receiver<Message>,
    mut _api_received_messages_tx: mpsc::Sender<Message>,
) {
    loop {
        api_sender_rx.recv().await;
    }
    // let timers = DelayQueue::new();
}

/*

for _ in 0..WINDOW_SIZE {
    todo!("Send packets");
}

let mut buf = [0; MTU];

let mut send_queue = self.send_queue;
let mut timers = self.timers;

loop {
    let expired_future = futures::future::poll_fn(|cx| timers.poll_expired(cx));

    tokio::select! {
        Ok((n, _)) = self.socket.recv_from(&mut buf) => {
            let _packet = Packet::deserialize(&buf[..n]);
            todo!()
        }
        Some(packet) = send_queue.recv() => {
            let len = packet.serialize(&mut buf);
            self.socket.send(&buf[..len]).await?;
        }
        Some(expired) = expired_future => {

        }
    }
}
*/
