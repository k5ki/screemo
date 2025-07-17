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
use tokio::{sync::broadcast::error::TryRecvError, task};

#[derive(Debug, Clone)]
struct Handler {
    qt: Arc<tokio::sync::mpsc::Sender<Message>>,
    tt: Arc<tokio::sync::broadcast::Sender<Message>>,
}

impl Handler {
    fn new(
        qt: Arc<tokio::sync::mpsc::Sender<Message>>,
        tt: Arc<tokio::sync::broadcast::Sender<Message>>,
    ) -> Self {
        Self { qt, tt }
    }
    async fn send(&self, text: &str) -> Result<()> {
        let msg = Message {
            text: text.to_string(),
        };
        self.qt
            .send(msg)
            .await
            .map_err(|e| anyhow::anyhow!("failed to send message: {}", e))?;
        Ok(())
    }
}

async fn echo(
    req: HttpRequest,
    stream: web::Payload,
    srv: web::Data<Arc<Handler>>,
) -> Result<HttpResponse, Error> {
    let (res, mut session, stream) = actix_ws::handle(&req, stream)?;

    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));

    let mut rx = srv.tt.subscribe();
    rt::spawn(async move {
        loop {
            match stream.try_next().await {
                Ok(Some(AggregatedMessage::Text(text))) => {
                    if !text.is_empty() {
                        srv.send(&text).await.unwrap();
                        session.text(format!("send: {}", text)).await.unwrap();
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
                Ok(msg) => {
                    session
                        .text(format!("received: {}", msg.text))
                        .await
                        .unwrap();
                }
                Err(TryRecvError::Empty) => {}
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
struct Message {
    text: String,
}

#[tokio::main]
async fn main() {
    let done = Arc::new(AtomicBool::new(false));

    let (qt, mut qr) = tokio::sync::mpsc::channel::<Message>(1000);
    let qt = Arc::new(qt);

    let (tt, mut _tr) = tokio::sync::broadcast::channel::<Message>(1000);
    let tt = Arc::new(tt);

    let done_clone = done.clone();
    let tt_clone = tt.clone();
    let fibre = task::spawn(async move {
        let done = done_clone;
        let tt = tt_clone;
        while !done.load(Ordering::Relaxed) {
            if let Ok(msg) = qr.try_recv() {
                tt.send(msg.clone()).unwrap();
            } else {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
    });

    HttpServer::new(move || {
        let handler = Handler::new(qt.clone(), tt.clone());
        App::new()
            .app_data(web::Data::new(Arc::new(handler)))
            .route("/ws", web::get().to(echo))
    })
    .bind(("127.0.0.1", 8080))
    .inspect_err(|_| {
        done.store(true, Ordering::Relaxed);
    })
    .unwrap()
    .run()
    .await
    .inspect_err(|_| {
        done.store(true, Ordering::Relaxed);
    })
    .unwrap();

    done.store(true, Ordering::Relaxed);
    fibre.await.unwrap();
}
