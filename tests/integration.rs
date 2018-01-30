extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate tokio_core;

use picoquic::{CMessage, Config, Context, SMessage};

use std::net::SocketAddr;
use std::thread;
use std::sync::mpsc::channel;
use std::fmt;

use futures::{Future, Sink, Stream as FStream};
use futures::sync::mpsc::unbounded;

use tokio_core::reactor::{Core, Handle};

use bytes::Bytes;

fn get_test_config() -> Config {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Config {
        cert_filename: format!("{}/tests/cert.pem", manifest_dir),
        key_filename: format!("{}/tests/key.pem", manifest_dir),
        reset_seed: None,
    }
}

fn create_context_and_evt_loop() -> (Context, Core) {
    let evt_loop = Core::new().expect("creates event loop");

    let context = Context::new(
        &([0, 0, 0, 0], 0).into(),
        &evt_loop.handle(),
        get_test_config(),
    ).expect("creates quic context");

    (context, evt_loop)
}

fn start_server_thread<F, R>(create_future: F) -> SocketAddr
where
    F: 'static + Send + FnOnce(Context, Handle) -> R,
    R: Future,
    <R as Future>::Error: fmt::Debug,
{
    let (send, recv) = channel();

    thread::spawn(move || {
        let (context, mut evt_loop) = create_context_and_evt_loop();

        send.send(context.local_addr())
            .expect("sends server socket addr");

        let handle = evt_loop.handle();
        evt_loop
            .run(create_future(context, handle))
            .expect("event loop spins on server context");
    });

    recv.recv().expect("receives server socket addr")
}

#[test]
fn server_start() {
    start_server_thread(|c, _| c.for_each(|_| Ok(())));
}

#[test]
fn client_connects_creates_stream_and_sends_data() {
    let send_data = "hello server";
    let (send, recv) = unbounded();

    let addr = start_server_thread(move |c, h| {
        c.for_each(move |c| {
            let h = h.clone();
            let send = send.clone();
            c.for_each(move |s| {
                let h = h.clone();
                let send = send.clone();
                match s {
                    CMessage::NewStream(s) => h.clone().spawn(s.for_each(move |m| {
                        match m {
                            SMessage::Data(data) => {
                                let _ = send.clone()
                                    .unbounded_send(String::from_utf8(data.to_vec()).unwrap());
                            }
                            _ => {}
                        };
                        Ok(())
                    }).map_err(|_| ())),
                    _ => {}
                };
                Ok(())
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop();

    let mut con = evt_loop
        .run(context.connect_to(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let stream = evt_loop
        .run(con.new_bidirectional_stream())
        .expect("creates stream");

    // The stream must not be dropped here!
    let _stream = evt_loop
        .run(stream.send(SMessage::Data(Bytes::from(send_data))))
        .unwrap();

    assert_eq!(
        send_data,
        evt_loop.run(recv.into_future()).unwrap().0.unwrap()
    );
}
