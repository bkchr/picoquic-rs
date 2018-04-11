extern crate bytes;
extern crate futures;
extern crate openssl;
extern crate picoquic;
extern crate timebomb;
extern crate tokio_core;

use picoquic::{default_verify_certificate, Config, Connection, ConnectionId, ConnectionType,
               Context, ErrorKind, FileFormat, NewStreamFuture, NewStreamHandle, SType, Stream,
               VerifyCertificate};

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;

use futures::sync::mpsc::unbounded;
use futures::{Future, Sink, Stream as FStream};

use tokio_core::reactor::{Core, Handle};

use bytes::BytesMut;

use openssl::error::ErrorStack;
use openssl::stack::StackRef;
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::{X509, X509Ref};

fn get_test_certs_path() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{}/tests/certs/", manifest_dir)
}

fn get_test_config() -> Config {
    let mut config = Config::new();
    config.set_cert_chain_filename(format!("{}device.test.crt", get_test_certs_path()));
    config.set_key_filename(format!("{}device.key", get_test_certs_path()));
    config
}

fn create_context_and_evt_loop_with_default_config() -> (Context, Core) {
    create_context_and_evt_loop(get_test_config())
}

fn create_context_and_evt_loop(config: Config) -> (Context, Core) {
    let evt_loop = Core::new().expect("creates event loop");

    let context = Context::new(&([0, 0, 0, 0], 0).into(), &evt_loop.handle(), config)
        .expect("creates quic context");

    (context, evt_loop)
}

fn start_server_thread_with_default_config<F, R>(create_future: F) -> SocketAddr
where
    F: 'static + Send + FnOnce(Context, Handle) -> R,
    R: Future,
    <R as Future>::Error: fmt::Debug,
{
    start_server_thread(|| get_test_config(), create_future)
}

fn start_server_thread<F, R, C>(create_config: C, create_future: F) -> SocketAddr
where
    F: 'static + Send + FnOnce(Context, Handle) -> R,
    R: Future,
    <R as Future>::Error: fmt::Debug,
    C: 'static + Send + FnOnce() -> Config,
{
    let (send, recv) = channel();

    thread::spawn(move || {
        let (context, mut evt_loop) = create_context_and_evt_loop(create_config());

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
    start_server_thread_with_default_config(|c, _| c.for_each(|_| Ok(())));
}

fn client_connects_creates_stream_and_sends_data<F, T, C>(
    client_config: Config,
    create_server_config: C,
    create_stream: F,
    check_type: T,
) where
    F: Fn(Connection) -> (NewStreamFuture, Connection),
    T: Fn(&Stream),
    C: 'static + Send + FnOnce() -> Config,
{
    let send_data = "hello server";
    let (send, recv) = unbounded();

    let addr = start_server_thread(create_server_config, move |c, _| {
        c.for_each(move |c| {
            assert_eq!(c.get_type(), ConnectionType::Incoming);
            let send = send.clone();
            c.for_each(move |s| {
                let send = send.clone();
                s.for_each(move |m| {
                    let _ = send.clone()
                        .unbounded_send(String::from_utf8(m.to_vec()).unwrap());
                    Ok(())
                })
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop(client_config);

    let con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");
    assert_eq!(con.get_type(), ConnectionType::Outgoing);

    let (new_stream, _con) = create_stream(con);
    let stream = evt_loop.run(new_stream).expect("creates stream");
    check_type(&stream);

    assert_eq!(
        stream.local_addr(),
        ([0, 0, 0, 0], context.local_addr().port()).into()
    );
    assert_eq!(stream.peer_addr(), ([127, 0, 0, 1], addr.port()).into());
    assert_ne!(stream.peer_addr(), stream.local_addr());

    // The stream must not be dropped here!
    let _stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    assert_eq!(
        send_data,
        evt_loop.run(recv.into_future()).unwrap().0.unwrap()
    );
}

fn client_connects_creates_bidirectional_stream_and_sends_data_impl<C>(
    client_config: Config,
    create_server_config: C,
) where
    C: 'static + Send + FnOnce() -> Config,
{
    client_connects_creates_stream_and_sends_data(
        client_config,
        create_server_config,
        |mut c| {
            let stream = c.new_bidirectional_stream();
            (stream, c)
        },
        |s| {
            assert!(match s.get_type() {
                SType::Bidirectional => true,
                _ => false,
            })
        },
    )
}

#[test]
fn client_connects_creates_bidirectional_stream_and_sends_data() {
    client_connects_creates_bidirectional_stream_and_sends_data_impl(get_test_config(), || {
        get_test_config()
    });
}

#[test]
fn client_connects_creates_unidirectional_stream_and_sends_data() {
    client_connects_creates_stream_and_sends_data(
        get_test_config(),
        || get_test_config(),
        |mut c| {
            let stream = c.new_unidirectional_stream();
            (stream, c)
        },
        |s| {
            assert!(match s.get_type() {
                SType::Unidirectional => true,
                _ => false,
            })
        },
    )
}

#[test]
fn connection_and_stream_closes_on_drop() {
    timebomb::timeout_ms(connection_and_stream_closes_on_drop_inner, 10000);
}

fn connection_and_stream_closes_on_drop_inner() {
    let send_data = "hello server";
    let (send, recv) = unbounded();

    let addr = start_server_thread_with_default_config(move |c, _| {
        c.for_each(move |c| {
            let send = send.clone();
            c.into_future().map_err(|e| e.0).and_then(move |(s, c)| {
                let send = send.clone();
                s.unwrap()
                    .into_future()
                    .map(move |_| {
                        let _ = send.clone().unbounded_send(true);
                        let _ = c;
                        ()
                    })
                    .map_err(|e| e.0)
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let mut con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let stream = evt_loop
        .run(con.new_bidirectional_stream())
        .expect("creates stream");

    let stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    assert!(evt_loop.run(recv.into_future()).unwrap().0.unwrap());

    assert!(match evt_loop
        .run(stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
        .unwrap()
    {
        None => true,
        _ => false,
    });

    assert!(match evt_loop
        .run(con.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
        .unwrap()
    {
        None => true,
        _ => false,
    });
}

fn start_server_that_sends_received_data_back<C>(create_config: C) -> SocketAddr
where
    C: 'static + Send + FnOnce() -> Config,
{
    start_server_thread(create_config, move |c, h| {
        c.for_each(move |c| {
            let h = h.clone();

            h.clone().spawn(c.for_each(move |s| {
                // Peer addr and local addr can not be the same
                assert_ne!(s.peer_addr(), s.local_addr());
                // The local address should be unspecified, as we do not set a specific address in
                // the tests.
                assert!(s.local_addr().ip().is_unspecified());

                h.clone().spawn(
                    s.into_future()
                        .map_err(|_| ())
                        .and_then(|(v, s)|{
                           s.send(v.unwrap())
                            .map_err(|_| ()) })
                        // we need to do a fake collect here, to prevent that the stream gets
                        // dropped to early.
                        .and_then(|s| s.collect().map_err(|_| ()))
                        .map(|_| ()),
                );
                Ok(())
            }).map_err(|_| ()));

            Ok(())
        })
    })
}

#[test]
fn open_multiple_streams_sends_data_and_recvs() {
    let send_data = "hello server";
    let expected_stream_count = 4;

    let addr = start_server_that_sends_received_data_back(|| get_test_config());

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let mut con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let mut streams = Vec::new();

    for i in 0..expected_stream_count {
        let stream = evt_loop
            .run(con.new_bidirectional_stream())
            .expect("creates stream");
        streams.push(
            evt_loop
                .run(stream.send(BytesMut::from(format!("{}{}", send_data, i))))
                .unwrap(),
        );
    }

    for (i, stream) in streams.iter_mut().enumerate() {
        assert_eq!(
            format!("{}{}", send_data, i),
            String::from_utf8(
                evt_loop
                    .run(stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
                    .unwrap()
                    .unwrap()
                    .to_vec(),
            ).unwrap()
        );
    }
}

fn open_stream_to_server_and_server_creates_new_stream_to_answer<F>(create_stream: F)
where
    F: 'static + Sync + Send + Fn(NewStreamHandle) -> NewStreamFuture,
{
    let create_stream = Arc::new(Box::new(create_stream));
    let send_data = "hello server";

    let create_stream2 = create_stream.clone();
    let addr = start_server_thread_with_default_config(move |c, _| {
        let create_stream = create_stream2;
        c.for_each(move |c| {
            let create_stream = create_stream.clone();
            let new_stream = c.get_new_stream_handle();

            c.for_each(move |s| {
                let create_stream = create_stream.clone();
                let new_stream = new_stream.clone();

                s.into_future()
                        .map_err(|e| e.0)
                        .and_then(move |(v, _)|
                                  create_stream(new_stream.clone())
                                  .and_then(move |s|
                                            s.send(v.unwrap())
                                            .map_err(|_| ErrorKind::Unknown.into())))
                        // we need to do a fake collect here, to prevent that the stream gets
                        // dropped to early.
                    .and_then(|s| s.collect().map_err(|_| ErrorKind::Unknown.into()))
                        .map(|_| ())
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let stream = evt_loop
        .run(create_stream(con.get_new_stream_handle()))
        .expect("creates stream");
    let _stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    let (new_stream, _con) = evt_loop
        .run(
            con.into_future()
                .map(|(m, c)| (m.unwrap(), c))
                .map_err(|(e, _)| e),
        )
        .unwrap();

    assert_eq!(
        send_data,
        String::from_utf8(
            evt_loop
                .run(new_stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
                .unwrap()
                .unwrap()
                .to_vec()
        ).unwrap()
    );
}

#[test]
fn open_uni_stream_to_server_and_server_creates_new_uni_stream_to_answer() {
    open_stream_to_server_and_server_creates_new_stream_to_answer(|mut h| {
        h.new_unidirectional_stream()
    });
}

#[test]
fn open_bi_stream_to_server_and_server_creates_new_bi_stream_to_answer() {
    open_stream_to_server_and_server_creates_new_stream_to_answer(|mut h| {
        h.new_bidirectional_stream()
    });
}

#[derive(Clone)]
struct VerifyCertificateImpl {
    counter: Arc<AtomicUsize>,
}

impl VerifyCertificateImpl {
    fn new() -> VerifyCertificateImpl {
        VerifyCertificateImpl {
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn increment(&self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }

    fn get(&self) -> usize {
        self.counter.load(Ordering::SeqCst)
    }
}

impl VerifyCertificate for VerifyCertificateImpl {
    fn verify(
        &mut self,
        _: ConnectionId,
        _: ConnectionType,
        cert: &X509Ref,
        chain: &StackRef<X509>,
    ) -> Result<bool, ErrorStack> {
        let ca_cert = include_bytes!("certs/ca.crt");
        let ca_cert = X509::from_pem(ca_cert)?;

        let mut store_bldr = X509StoreBuilder::new().unwrap();
        store_bldr.add_cert(ca_cert).unwrap();
        let store = store_bldr.build();

        let res = default_verify_certificate(cert, chain, &store);

        self.increment();

        res
    }
}

fn verify_certificate_callback_is_called_and_certificate_is_verified(
    client_cert: String,
    client_key: String,
) {
    let send_data = "hello server";
    let call_counter = VerifyCertificateImpl::new();

    let mut client_config = get_test_config();
    client_config.set_verify_certificate_handler(call_counter.clone());
    client_config.set_cert_chain_filename(client_cert);
    client_config.set_key_filename(client_key);

    let server_call_counter = call_counter.clone();
    let addr = start_server_that_sends_received_data_back(move || {
        let mut server_config = get_test_config();
        server_config.set_verify_certificate_handler(server_call_counter);
        server_config.enable_client_authentication();

        server_config
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop(client_config);

    let mut con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let stream = evt_loop
        .run(con.new_bidirectional_stream())
        .expect("creates stream");
    let stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    assert_eq!(
        send_data,
        &String::from_utf8(
            evt_loop
                .run(stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
                .unwrap()
                .unwrap()
                .to_vec()
        ).unwrap()
    );

    assert_eq!(call_counter.get(), 2);
}

#[test]
fn verify_certificate_callback_is_called_and_certificate_verification_succeeds() {
    verify_certificate_callback_is_called_and_certificate_is_verified(
        format!("{}device.test.crt", get_test_certs_path()),
        format!("{}device.key", get_test_certs_path()),
    )
}

#[test]
#[should_panic(expected = "An error occurred in the TLS handshake.")]
fn verify_certificate_callback_is_called_and_certificate_verification_fails() {
    verify_certificate_callback_is_called_and_certificate_is_verified(
        format!("{}device.invalid.crt", get_test_certs_path()),
        format!("{}device.key", get_test_certs_path()),
    )
}

#[test]
fn set_certificate_and_key_from_memory() {
    client_connects_creates_bidirectional_stream_and_sends_data_impl(get_test_config(), || {
        let cert = include_bytes!("certs/device.test.crt");
        let key = include_bytes!("certs/device.key");

        let mut config = get_test_config();
        config.cert_chain_filename = None;
        config.key_filename = None;
        config.set_cert_chain(vec![cert.to_vec()], FileFormat::PEM);
        config.set_key(key.to_vec(), FileFormat::PEM);
        config
    });
}
