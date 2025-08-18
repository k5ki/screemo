use actix_web::{
    App, Error, HttpRequest, HttpResponse, HttpServer,
    rt::{self},
    web,
};
use actix_ws::AggregatedMessage;
use anyhow::Result;
use futures_util::TryStreamExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{sync::broadcast, sync::mpsc};

#[derive(Debug, Clone)]
struct Socket {
    qt: Arc<mpsc::Sender<Command>>,
    tt: Arc<broadcast::Sender<Command>>,
}

impl Socket {
    pub fn new(qt: Arc<mpsc::Sender<Command>>, tt: Arc<broadcast::Sender<Command>>) -> Self {
        Self { qt, tt }
    }
    pub fn receiver(&self) -> broadcast::Receiver<Command> {
        self.tt.subscribe()
    }
    pub async fn send(&self, p: Command) -> Result<()> {
        self.qt
            .send(p)
            .await
            .map_err(|e| anyhow::anyhow!("failed to send message: {}", e))?;
        Ok(())
    }
}

async fn terminal(
    req: HttpRequest,
    stream: web::Payload,
    sock: web::Data<Arc<Socket>>,
) -> Result<HttpResponse, Error> {
    let (res, mut session, stream) = actix_ws::handle(&req, stream)?;

    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));

    let mut rx = sock.receiver();
    rt::spawn(async move {
        loop {
            match stream.try_next().await {
                Ok(Some(AggregatedMessage::Text(text))) => {
                    // TODO: parse JSON
                    if !text.is_empty() {
                        srv.send(Command::Connect(CommandMessage {})).await.unwrap();
                    }
                }
                Ok(Some(AggregatedMessage::Ping(msg))) => {
                    session.pong(&msg).await.unwrap();
                }
                Err(_) => {
                    session.close(None).await.unwrap();
                    break;
                }
                _ => {}
            }
            match rx.try_recv() {
                Ok(packet) => {
                    // TODO: serialize packet to JSON
                    session
                        .text(format!("received: {}", msg.text))
                        .await
                        .unwrap();
                }
                Err(broadcast::error::TryRecvError::Empty) => {}
                _ => {
                    session.close(None).await.unwrap();
                    break;
                }
            }
        }
    });
    Ok(res)
}

#[derive(Debug, Clone)]
enum Command {
    Connect(CommandMessage),
    Accept(CommandMessage),
    Register(CommandMessage),
    Discovered(CommandMessage),
    SendIce(CommandMessage),
    Cancel(CommandMessage),
}

#[derive(Debug, Clone)]
enum Actor {
    System,
    User(String),
    TempUser(String),
}

#[derive(Debug, Clone)]
struct CommandMessage {
    from: Actor,
    to: Actor,
    body: String,
}

#[derive(Debug, Clone)]
enum IncomingMessage {
    Connect,
    Register(String),
    SendIce(String),
    Cancel,
}

#[derive(Debug, Clone)]
enum OutgoingMessage {
    Accept,
    Discovered(String),
    SendIce(String),
    Canceled,
    Error(String),
}

fn spawn_exchanger(
    done: Arc<AtomicBool>,
    t: Arc<broadcast::Sender<Command>>,
    mut q: mpsc::Receiver<Command>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while !done.load(Ordering::Relaxed) {
            if let Ok(msg) = q.try_recv() {
                t.send(msg.clone()).unwrap();
            } else {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
    })
}

#[tokio::main]
async fn main() {
    let done = Arc::new(AtomicBool::new(false));

    let (qt, qr) = mpsc::channel::<Command>(1000);
    let qt = Arc::new(qt);

    let (tt, mut _tr) = broadcast::channel::<Command>(1000);
    let tt = Arc::new(tt);

    let exchanger = spawn_exchanger(done.clone(), tt.clone(), qr);
    let server = HttpServer::new(move || {
        let handler = Handler::new(qt.clone(), tt.clone());
        App::new()
            .app_data(web::Data::new(Arc::new(handler)))
            .route("/ws", web::get().to(terminal))
    })
    .bind(("127.0.0.1", 8080))
    .inspect_err(|_| done.store(true, Ordering::Relaxed))
    .unwrap();

    server.run().await;
    done.store(true, Ordering::Relaxed);
    exchanger.await;
}
