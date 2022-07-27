use futures::future::{self};
use futures::{SinkExt, Stream, StreamExt, TryStreamExt};
use serde_json::{from_str, to_value, Value};
use short_crypt::ShortCrypt;
use std::convert::Infallible;
use std::env;
use std::error::Error;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::broadcast::{self, Sender};
use tokio::task::spawn;
use tokio_stream::wrappers::BroadcastStream;
use warp::{reject, Rejection};
use warp::{
    sse::Event,
    ws::{Message, Ws},
    Filter, Reply,
};

type Subject = (String, String);

fn stream_filtered_for_subject<T: Stream<Item = Result<Subject, impl Error>>>(
    subject: String,
    subject_stream: T,
) -> impl Stream<Item = Result<String, warp::Error>> {
    subject_stream.filter_map(move |message_result| {
        if let Ok((message_subject, value)) = message_result {
            if message_subject == subject {
                return future::ready(Some(Ok(value)));
            }
        }
        future::ready(None)
    })
}

fn compose_ws_filter<
    Base: Filter<Extract = (String, Arc<Sender<Subject>>), Error = Rejection> + Clone,
>(
    base: Base,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    base.and(warp::ws())
        .map(|subject: String, sender: Arc<Sender<Subject>>, ws: Ws| {
            ws.on_upgrade(|socket| async move {
                let (sink, mut stream) = socket.split();

                let subject_stream = BroadcastStream::new(sender.subscribe());
                let subject_copy = subject.clone();
                let mut sink_wrap = sink.with(|value: String| future::ok(Message::text(value)));

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
                            .send((to, value.to_string()))
                            .map_err(|e| eprintln!("{:?}", e))
                            .unwrap();
                    }
                }
                listen.abort()
            })
        })
}

fn compose_sse_filter<
    BaseFilter: Filter<Extract = (String, Arc<Sender<Subject>>), Error = Rejection> + Clone,
>(
    base: BaseFilter,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    base.and(warp::get())
        .map(|subject, sender: Arc<Sender<Subject>>| {
            warp::sse::reply(
                warp::sse::keep_alive().stream(
                    stream_filtered_for_subject(subject, BroadcastStream::new(sender.subscribe()))
                        .map_ok(|v| Event::default().data(v)),
                ),
            )
        })
}

async fn decrypt_subject(
    subject: String,
    short_crypt: Option<Arc<ShortCrypt>>,
) -> Result<String, Rejection> {
    match short_crypt {
        Some(c) => c
            .decrypt_url_component(subject)
            .map_err(|_err| reject::not_found())
            .and_then(move |bytes| String::from_utf8(bytes).map_err(|_| reject::not_found())),
        None => Err(reject::not_found()),
    }
}

fn with_value<T: Clone + Send>(
    value: T,
) -> impl Filter<Extract = (T,), Error = Infallible> + Clone {
    warp::any().map(move || value.clone())
}

fn with_tail_path() -> impl Filter<Extract = (String,), Error = Rejection> + Clone {
    warp::path::tail().and_then(|tail: warp::path::Tail| {
        let rest = tail.as_str();
        match rest.is_empty() {
            false => future::ok(String::from(rest)),
            true => future::err(reject::not_found()),
        }
    })
}

fn get_root_filter(crypto_key: Option<String>) -> impl Filter<Extract = impl Reply> + Clone {
    let sender = Arc::new(broadcast::channel::<Subject>(100).0);

    let ws = warp::path("ws")
        .and(with_tail_path())
        .and(with_value(sender.clone()));

    let sse = warp::path("sse")
        .and(with_tail_path())
        .and(with_value(sender.clone()));

    let ws_short_crypt = crypto_key.map(ShortCrypt::new).map(Arc::new);
    let sse_short_crypt = ws_short_crypt.clone();
    let ws_encrypted = warp::path("code")
        .and(warp::path("ws"))
        .and(with_tail_path())
        .and_then(move |s| decrypt_subject(s, ws_short_crypt.clone()))
        .and(with_value(sender.clone()));
    let sse_encrypted = warp::path("code")
        .and(warp::path("sse"))
        .and(with_tail_path())
        .and_then(move |s| decrypt_subject(s, sse_short_crypt.clone()))
        .and(with_value(sender.clone()));

    let http_post_connect = warp::path("http")
        .and(with_tail_path())
        .and(with_value(sender.clone()))
        .and(warp::post())
        .and(warp::body::json())
        .map(|subject, sender: Arc<Sender<Subject>>, body: Value| {
            sender.send((subject, body.to_string())).ok();
            warp::reply()
        });

    let http_encrypt_subject = warp::path("encrypt")
        .and(warp::path::param::<String>())
        .and(with_tail_path())
        .and(warp::get())
        .map(|key, subject| ShortCrypt::new(key).encrypt_to_url_component(&subject));

    let cors = warp::cors()
        .allow_any_origin()
        .allow_header("content-type")
        .allow_methods(vec!["GET", "POST"]);

    compose_ws_filter(ws)
        .or(compose_ws_filter(ws_encrypted))
        .or(compose_sse_filter(sse))
        .or(compose_sse_filter(sse_encrypted))
        .or(http_post_connect)
        .or(http_encrypt_subject)
        .with(&cors)
}

fn parse_env_arg<T: FromStr>(matches: &[&str]) -> Option<T> {
    env::args()
        .skip_while(|s| matches.iter().all(|m| !s.starts_with(m)))
        .next()
        .and_then(|s| s.split_once('=').and_then(|(_, val)| val.parse().ok()))
}

#[tokio::main]
async fn main() {
    let port = parse_env_arg(&["--port", "-p"]).unwrap_or(3030);
    let crypto_key = parse_env_arg::<String>(&["--key", "-k"]);
    println!(
        "Serving {} encryption key at port {}",
        match crypto_key {
            Some(_) => "with",
            None => "without",
        },
        port
    );
    warp::serve(get_root_filter(crypto_key))
        .run(([0, 0, 0, 0], port))
        .await;
    println!("Stopped serving")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::get_root_filter;
    use futures::{select, FutureExt, TryFutureExt};
    use serde_json::{from_str, json, Value};
    use warp::{test::WsClient, Filter, Reply};

    const SUBJECT: &str = "subject";
    const MESSAGE: &str = "message from sender";

    async fn timeout<F: futures::Future>(future: F) -> F::Output {
        select! {
            _ = tokio::time::sleep(Duration::from_millis(100)).fuse() => panic!("Timed out"),
            r = future.fuse() => r
        }
    }

    async fn connect_ws<F>(filter: F) -> WsClient
    where
        F: Filter + Clone + Send + Sync + 'static,
        F::Extract: Reply,
    {
        warp::test::ws()
            .path(&format!("/ws/{SUBJECT}"))
            .handshake(filter)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ws_to_ws_message() {
        let filter = get_root_filter(None);
        let mut receiver = connect_ws(filter.clone()).await;
        let mut sender = connect_ws(filter).await;
        let receive = receiver.recv().map_ok(|s| {
            let json = from_str::<Value>(s.to_str().expect("No text received")).unwrap();
            let received_message = json.get("message").unwrap();
            assert_eq!(MESSAGE, received_message, "Message did not match");
        });
        sender
            .send_text(json!({"to": SUBJECT, "message": MESSAGE}).to_string())
            .await;
        receive.await.unwrap();
    }

    #[tokio::test]
    async fn http_to_ws_message() {
        let filter = get_root_filter(None);
        let mut receiver = connect_ws(filter.clone()).await;
        let receive = receiver.recv().map_ok(|s| {
            let json = from_str::<Value>(s.to_str().expect("No text received")).unwrap();
            let received_message = json.get("message").unwrap();
            assert_eq!(MESSAGE, received_message, "Message did not match");
        });
        let post = warp::test::request()
            .method("POST")
            .path(&format!("/http/{SUBJECT}"))
            .json(&json!({ "message": MESSAGE }))
            .reply(&filter)
            .await
            .status();
        assert_eq!(200, post);
        timeout(receive).await.unwrap();
    }
}
