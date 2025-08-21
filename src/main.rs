use actix_web::{
    App, Error, HttpRequest, HttpResponse, HttpServer,
    rt::{self},
    web,
};
use actix_ws::AggregatedMessage;
use anyhow::Result;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
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
                Ok(Some(AggregatedMessage::Text(text))) => match serde_json::from_str(&text) {
                    Ok(IncomingMessage::Connect) => {
                        sock.send(Command::Connect(CommandMessage {
                            from: Actor::TempUser("".to_string()),
                            to: Actor::System,
                            body: "".to_string(),
                        }))
                        .await
                        .unwrap();
                    }
                    Ok(IncomingMessage::Register(name)) => {
                        sock.send(Command::Connect(CommandMessage {
                            from: Actor::TempUser("".to_string()),
                            to: Actor::System,
                            body: name,
                        }))
                        .await
                        .unwrap();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let om =
                            serde_json::to_string(&OutgoingMessage::Error(e.to_string())).unwrap();
                        session.text(om).await.unwrap();
                    }
                },
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
                #[allow(clippy::single_match)]
                Ok(cmd) => match cmd {
                    Command::Accept(message) => {
                        if message.to != Actor::TempUser("".to_string()) {
                            break;
                        }
                        session
                            .text(serde_json::to_string(&OutgoingMessage::Accept).unwrap())
                            .await
                            .unwrap();
                    }
                    _ => {}
                },
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Serialize, Deserialize, Debug, Clone)]
enum IncomingMessage {
    Connect,
    Register(String),
    SendIce(String),
    Cancel,
}

#[derive(Serialize, Debug, Clone)]
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
            if let Ok(cmd) = q.try_recv() {
                #[allow(clippy::single_match)]
                match cmd {
                    Command::Connect(message) => {
                        t.send(Command::Accept(CommandMessage {
                            from: Actor::System,
                            to: message.from.clone(),
                            body: "accepted".to_string(),
                        }))
                        .unwrap();
                    }
                    _ => {}
                }
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
        let sock = Socket::new(qt.clone(), tt.clone());
        App::new()
            .app_data(web::Data::new(Arc::new(sock)))
            .route("/ws", web::get().to(terminal))
    })
    .bind(("127.0.0.1", 8080))
    .inspect_err(|_| done.store(true, Ordering::Relaxed))
    .unwrap();

    server.run().await;
    done.store(true, Ordering::Relaxed);
    exchanger.await;
}
