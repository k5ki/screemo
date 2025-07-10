use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use actix_web::{
    App, Error, HttpRequest, HttpResponse, HttpServer,
    rt::{self},
    web,
};
use actix_ws::AggregatedMessage;

use futures_util::StreamExt;
use tokio::task;

#[derive(Debug, Clone)]
struct Handler {
    counter: Arc<Mutex<i64>>,
}

impl Handler {
    fn new(counter: Arc<Mutex<i64>>) -> Self {
        Self { counter }
    }
    fn counter(&self) -> i64 {
        let a = self.counter.lock().unwrap();
        *a
    }
    fn increment(&self) {
        let mut a = self.counter.lock().unwrap();
        *a += 1;
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
        // aggregate continuation frames up to 1MiB
        .max_continuation_size(2_usize.pow(20));

    // start task but don't wait for it
    rt::spawn(async move {
        // receive messages from websocket
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(AggregatedMessage::Text(_)) => {
                    // echo text message
                    srv.increment();
                    session
                        .text(format!("count: {}", srv.counter()))
                        .await
                        .unwrap();
                }

                Ok(AggregatedMessage::Ping(msg)) => {
                    // respond to PING frame with PONG frame
                    session.pong(&msg).await.unwrap();
                }

                _ => {
                    session.close(None).await.unwrap();
                    break;
                }
            }
        }
    });

    // respond immediately with response connected to WS session
    Ok(res)
}

#[tokio::main]
async fn main() {
    let done = Arc::new(AtomicBool::new(false));
    let counter = Arc::new(Mutex::new(10000000000000));

    let done_clone = done.clone();
    let counter_clone = counter.clone();
    let fibre = task::spawn(async move {
        let done = done_clone;
        let counter = counter_clone;
        while !done.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let mut c = counter.lock().unwrap();
            *c += 10000000;
        }
    });

    HttpServer::new(move || {
        let handler = Handler::new(counter.clone());
        App::new()
            .app_data(web::Data::new(Arc::new(handler)))
            .route("/echo", web::get().to(echo))
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
