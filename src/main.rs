use futures::future::{self};
use futures::{SinkExt, Stream, StreamExt, TryStreamExt};
use serde_json::{from_str, to_value, Value};
use std::convert::Infallible;
use std::env;
use std::error::Error;
use std::sync::Arc;
use tokio::sync::broadcast::{self, Sender};
use tokio::task::spawn;
use tokio_stream::wrappers::BroadcastStream;
use warp::sse::Event;
use warp::ws::Message;
use warp::{
    ws::{WebSocket, Ws},
    Filter,
};

type Subject = (String, Value);

fn stream_filtered_for_subject<T: Stream<Item = Result<Subject, impl Error>>>(
    subject: String,
    subject_stream: T,
) -> impl Stream<Item = Result<Value, warp::Error>> {
    subject_stream.filter_map(move |message_result| {
        if let Ok((message_subject, value)) = message_result {
            if message_subject == subject {
                return future::ready(Some(Ok(value)));
            }
        }
        future::ready(None)
    })
}

async fn connect_websocket(subject: String, socket: WebSocket, sender: Arc<Sender<Subject>>) {
    let (sink, mut stream) = socket.split();

    let subject_stream = BroadcastStream::new(sender.subscribe());
    let subject_copy = subject.clone();
    let mut sink_wrap = sink.with(|value: Value| future::ok(Message::text(value.to_string())));

    let listen = spawn(async move {
        sink_wrap
            .send_all(&mut stream_filtered_for_subject(
                subject_copy,
                subject_stream,
            ))
            .await
    });
    while let Some(message) = stream.next().await {
        let format_send = || {
            let message_value = message.ok()?;
            let string_message = message_value.to_str().ok()?;
            let mut json: Value = from_str(string_message).ok()?;
            let object = json.as_object_mut()?;
            let to = object.remove("to")?;
            let actual_to = to.as_str()?;
            object.insert("from".to_string(), to_value(&subject).ok()?);
            Some((actual_to.to_string(), to_value(object).ok()?))
        };
        if let Some((to, value)) = format_send() {
            sender
                .send((to, value))
                .map_err(|e| eprintln!("{:?}", e))
                .unwrap();
        }
    }
    listen.abort()
}

fn with_value<T: Clone + Send>(
    value: T,
) -> impl Filter<Extract = (T,), Error = Infallible> + Clone {
    warp::any().map(move || value.clone())
}

#[tokio::main]
async fn main() {
    let sender = Arc::new(broadcast::channel::<Subject>(100).0);
    let cors = warp::cors()
        .allow_any_origin()
        .allow_header("content-type")
        .allow_methods(vec!["GET", "POST"]);

    let websocket_connect = warp::path!("ws" / String)
        .and(with_value(sender.clone()))
        .and(warp::ws())
        .map(|subject, sender, ws: Ws| {
            ws.on_upgrade(|socket| connect_websocket(subject, socket, sender))
        });

    let sse_connect = warp::path!("sse" / String)
        .and(with_value(sender.clone()))
        .and(warp::get())
        .map(|subject, sender: Arc<Sender<Subject>>| {
            warp::sse::reply(
                warp::sse::keep_alive().stream(
                    stream_filtered_for_subject(subject, BroadcastStream::new(sender.subscribe()))
                        .map_ok(|v| Event::default().json_data(v).unwrap()),
                ),
            )
        })
        .with(&cors);

    let http_post_connect = warp::path!("http" / String)
        .and(with_value(sender.clone()))
        .and(warp::post())
        .and(warp::body::json())
        .map(|subject, sender: Arc<Sender<Subject>>, body: Value| {
            sender.send((subject, body)).ok();
            warp::reply()
        })
        .with(&cors);

    let port = env::args()
        .skip_while(|s| s != "-p" && s != "--port")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3030);
    println!("Starting server on port {0}", port);

    warp::serve(
        warp::any()
            .and(websocket_connect)
            .or(sse_connect)
            .or(http_post_connect),
    )
    .run(([0, 0, 0, 0], port))
    .await;
}
