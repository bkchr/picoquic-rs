use futures;

use timebomb;
use tokio;

use picoquic::{
    default_verify_certificate, Config, Connection, ConnectionId, ConnectionType, Context, Error,
    ErrorKind, FileFormat, NewStreamFuture, NewStreamHandle, SType, Stream, VerifyCertificate,
};

use std::{
    cmp, fmt,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use futures::{future, stream, sync::mpsc::unbounded, Future, Sink, Stream as FStream};

use bytes::{Bytes, BytesMut};

use openssl::{
    error::ErrorStack,
    stack::StackRef,
    x509::{store::X509StoreBuilder, X509Ref, X509},
};

use tokio::{
    runtime::{Runtime, TaskExecutor},
    timer::{Delay, Interval},
};

use rand::{self, Rng};

const TEST_SERVER_NAME: &str = "picoquic.test";

fn get_test_certs_path() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{}/tests/certs/", manifest_dir)
}

fn get_test_config() -> Config {
    let mut config = Config::new();
    config.set_certificate_chain_filename(format!("{}device.test.crt", get_test_certs_path()));
    config.set_private_key_filename(format!("{}device.key", get_test_certs_path()));
    config.set_root_certificate_filename(format!("{}ca.crt", get_test_certs_path()));
    config
}

fn create_context_and_evt_loop_with_default_config() -> (Context, Runtime) {
    create_context_and_evt_loop(get_test_config())
}

fn create_context_and_evt_loop(config: Config) -> (Context, Runtime) {
    let evt_loop = Runtime::new().expect("creates event loop");

    let context = Context::new(&([0, 0, 0, 0], 0).into(), evt_loop.executor(), config)
        .expect("creates quic context");

    (context, evt_loop)
}

fn start_server_thread_with_default_config<F, R>(
    executor: TaskExecutor,
    create_future: F,
) -> SocketAddr
where
    F: 'static + Send + FnOnce(Context) -> R,
    R: Future + Send + 'static,
    <R as Future>::Item: Send + 'static,
    <R as Future>::Error: fmt::Debug + Send + 'static,
{
    start_server_thread(executor, get_test_config, create_future)
}

fn start_server_thread<F, R, C>(
    executor: TaskExecutor,
    create_config: C,
    create_future: F,
) -> SocketAddr
where
    F: 'static + Send + FnOnce(Context) -> R,
    R: Future + Send + 'static,
    <R as Future>::Error: fmt::Debug + Send + 'static,
    <R as Future>::Item: Send + 'static,
    C: 'static + Send + FnOnce() -> Config,
{
    let context = Context::new(&([0, 0, 0, 0], 0).into(), executor.clone(), create_config())
        .expect("creates quic context");

    let local_addr = context.local_addr();

    executor.spawn(create_future(context).map_err(|e| panic!(e)).map(|_| ()));

    local_addr
}

#[test]
fn server_start() {
    let runtime = Runtime::new().unwrap();
    start_server_thread_with_default_config(runtime.executor(), |c| c.for_each(|_| Ok(())));
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
    let (mut context, mut evt_loop) = create_context_and_evt_loop(client_config);

    let addr = start_server_thread(evt_loop.executor(), create_server_config, move |c| {
        c.for_each(move |c| {
            assert_eq!(c.get_type(), ConnectionType::Incoming);
            let send = send.clone();
            c.for_each(move |s| {
                let send = send.clone();
                s.for_each(move |m| {
                    let _ = send
                        .clone()
                        .unbounded_send(String::from_utf8(m.to_vec()).unwrap());
                    Ok(())
                })
            })
        })
    });

    let con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");
    assert_eq!(con.get_type(), ConnectionType::Outgoing);

    let (new_stream, _con) = create_stream(con);
    let stream = evt_loop.block_on(new_stream).expect("creates stream");
    check_type(&stream);

    assert_eq!(
        stream.local_addr(),
        ([0, 0, 0, 0], context.local_addr().port()).into()
    );
    assert_eq!(stream.peer_addr(), ([127, 0, 0, 1], addr.port()).into());
    assert_ne!(stream.peer_addr(), stream.local_addr());

    let _stream = evt_loop
        .block_on(stream.send(Bytes::from(send_data)))
        .unwrap();

    assert_eq!(
        send_data,
        evt_loop.block_on(recv.into_future()).unwrap().0.unwrap()
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
        get_test_config,
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
fn empty_stream_reset_and_no_more_send_on_drop() {
    timebomb::timeout_ms(empty_stream_reset_and_no_more_send_on_drop_inner, 10000);
}

fn empty_stream_reset_and_no_more_send_on_drop_inner() {
    let send_data = "hello server";

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let addr = start_server_thread_with_default_config(evt_loop.executor(), move |c| {
        c.for_each(move |c| {
            tokio::spawn(
                c.for_each(move |s| s.into_future().map(|_| ()).map_err(|e| e.0))
                    .map_err(|_| ()),
            );

            Ok(())
        })
    });

    let mut con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");

    let stream = evt_loop
        .block_on(con.new_bidirectional_stream())
        .expect("creates stream");

    let stream = evt_loop
        .block_on(stream.send(Bytes::from(send_data)))
        .unwrap();

    let (result, stream) = evt_loop
        .block_on(stream.into_future().map_err(|(e, _)| e))
        .unwrap();

    assert_eq!(result, None);
    assert!(stream.is_reset());
    // Send data in a loop, as `stop_sending` is processed after `reset` and to prevent race
    // conditions, we need to try sending multiple times.
    assert!(evt_loop
        .block_on(futures::lazy(move || stream.send_all(
            Interval::new(Instant::now(), Duration::from_millis(100))
                .map_err(|_| Error::from(ErrorKind::Unknown))
                .map(|_| Bytes::from("error"))
        )))
        .is_err());
}

#[test]
fn none_empty_stream_set_fin_bit_on_drop() {
    timebomb::timeout_ms(none_empty_stream_set_fin_bit_on_drop_inner, 10000);
}

fn none_empty_stream_set_fin_bit_on_drop_inner() {
    let send_data = "hello server";

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let addr = start_server_thread_with_default_config(evt_loop.executor(), move |c| {
        c.for_each(move |c| {
            tokio::spawn(
                c.for_each(move |s| s.send(Bytes::from(send_data)).map(|_| ()))
                    .map_err(|_| ()),
            );

            Ok(())
        })
    });

    let mut con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");

    let stream = evt_loop
        .block_on(
            con.new_bidirectional_stream()
                .and_then(move |s| s.send(Bytes::from(send_data))),
        )
        .expect("creates stream");

    let (result, stream) = evt_loop
        .block_on(stream.into_future().map_err(|(e, _)| e))
        .unwrap();

    assert_eq!(
        send_data,
        &String::from_utf8(result.unwrap().to_vec()).unwrap()
    );

    let (result, stream) = evt_loop
        .block_on(stream.into_future().map_err(|(e, _)| e))
        .unwrap();
    assert_eq!(result, None);

    assert!(!stream.is_reset());
}

fn start_server_that_sends_received_data_back<C>(
    executor: TaskExecutor,
    create_config: C,
) -> SocketAddr
where
    C: 'static + Send + FnOnce() -> Config,
{
    start_server_thread(executor, create_config, move |c| {
        c.for_each(move |c| {
            tokio::spawn(
                c.for_each(move |s| {
                    // Peer addr and local addr can not be the same
                    assert_ne!(s.peer_addr(), s.local_addr());
                    // The local address should be unspecified, as we do not set a specific address in
                    // the tests.
                    assert!(s.local_addr().ip().is_unspecified());

                    let (send, recv) = s.split();

                    tokio::spawn(
                        send.send_all(recv.map(BytesMut::freeze))
                            .map(|_| ())
                            .map_err(|_| ()),
                    );
                    Ok(())
                })
                .map_err(|_| ()),
            );

            Ok(())
        })
    })
}

#[test]
fn open_multiple_streams_sends_data_and_recvs() {
    let send_data = "hello server";
    let stream_count = 4;
    let mut data = Vec::new();

    for i in 0..stream_count {
        data.push(vec![Bytes::from(format!("{}{}", send_data, i))]);
    }

    open_multiple_streams_sends_data_and_recvs_impl(data);
}

fn open_multiple_streams_sends_data_and_recvs_impl(data: Vec<Vec<Bytes>>) {
    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();
    let addr = start_server_that_sends_received_data_back(evt_loop.executor(), get_test_config);

    let mut con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");

    let mut streams = Vec::new();

    for send in &data {
        let stream = evt_loop
            .block_on(con.new_bidirectional_stream())
            .expect("creates stream");
        streams.push(
            stream
                .send_all(stream::iter_ok::<_, Error>(send.clone().into_iter()))
                .map(|v| v.0),
        );
    }

    let streams = evt_loop.block_on(future::join_all(streams)).unwrap();

    for (stream, data) in streams.into_iter().zip(data.into_iter()) {
        let all_data = data.into_iter().fold(Vec::new(), |mut v, d| {
            v.extend(&d);
            v
        });
        let all_len = all_data.len();
        let res = evt_loop
            .block_on(
                stream
                    .map_err(|_| Vec::new())
                    .fold(Vec::new(), move |mut v, b| {
                        v.extend(&b);

                        if v.len() < all_len {
                            future::ok(v)
                        } else {
                            future::err(v)
                        }
                    }),
            )
            .err()
            .unwrap();

        assert_eq!(&all_data, &res);
    }
}

#[test]
fn open_multiple_streams_sends_1mb_data_and_recvs() {
    let stream_count = 4;
    let mut data = Vec::new();

    for _ in 0..stream_count {
        let mut send_data = vec![0; 1024 * 1024];
        rand::thread_rng().fill(&mut send_data[..]);
        data.push(vec![Bytes::from(send_data)]);
    }

    open_multiple_streams_sends_data_and_recvs_impl(data);
}

fn create_chunked_data(size: usize) -> Vec<Bytes> {
    let mut send_data = vec![0; size];
    rand::thread_rng().fill(&mut send_data[..]);
    let chunks = rand::thread_rng().gen_range(100, 1000);
    let chunk_size = (send_data.len() + chunks - 1) / chunks;
    (0..chunks)
        .fold((Vec::new(), 0), |(mut v, i), _| {
            v.push(Bytes::from(
                &send_data[i..cmp::min(i + chunk_size, send_data.len())],
            ));
            (v, i + chunk_size)
        })
        .0
}

#[test]
fn open_multiple_streams_sends_1mb_data_in_chunks_and_recvs() {
    let stream_count = 4;
    let mut data = Vec::new();

    for _ in 0..stream_count {
        data.push(create_chunked_data(1024 * 1024));
    }

    open_multiple_streams_sends_data_and_recvs_impl(data);
}

#[test]
fn open_stream_send_data_decrease_send_channel_size_send_data_and_recv_result() {
    let send_data = create_chunked_data(1024 * 1024);

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();
    let addr = start_server_that_sends_received_data_back(evt_loop.executor(), get_test_config);

    let mut con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");

    let send_data_inner = send_data.clone();
    let mut stream = evt_loop
        .block_on(con.new_bidirectional_stream().and_then(move |stream| {
            stream
                .send_all(stream::iter_ok::<_, Error>(send_data_inner.into_iter()))
                .map(|v| v.0)
        }))
        .expect("creates stream and sends data");

    stream.set_send_channel_size(1);

    let stream = evt_loop
        .block_on(
            stream
                .send_all(stream::iter_ok::<_, Error>(send_data.clone().into_iter()))
                .map(|v| v.0),
        )
        .expect("sends data with smaller send channel size");

    let mut all_data = send_data.into_iter().fold(Vec::new(), |mut v, d| {
        v.extend(&d);
        v
    });
    // we send the data twice
    all_data.extend(all_data.clone().into_iter());

    let all_len = all_data.len();
    let res = evt_loop
        .block_on(
            stream
                .map_err(|_| Vec::new())
                .fold(Vec::new(), move |mut v, b| {
                    v.extend(&b);

                    if v.len() < all_len {
                        future::ok(v)
                    } else {
                        future::err(v)
                    }
                }),
        )
        .err()
        .unwrap();

    assert_eq!(&all_data, &res);
}

fn open_stream_to_server_and_server_creates_new_stream_to_answer<F>(create_stream: F)
where
    F: 'static + Sync + Send + Fn(NewStreamHandle) -> NewStreamFuture,
{
    let create_stream = Arc::new(Box::new(create_stream));
    let send_data = "hello server";

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let create_stream2 = create_stream.clone();
    let addr = start_server_thread_with_default_config(evt_loop.executor(), move |c| {
        let create_stream = create_stream2;
        c.for_each(move |c| {
            let create_stream = create_stream.clone();
            let new_stream = c.get_new_stream_handle();

            c.for_each(move |incoming_stream| {
                let create_stream = create_stream.clone();
                let new_stream = new_stream.clone();

                create_stream(new_stream.clone())
                    .and_then(move |s| s.send_all(incoming_stream.map(BytesMut::freeze)))
                    .map(|_| ())
            })
        })
    });

    let con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");

    let stream = evt_loop
        .block_on(create_stream(con.get_new_stream_handle()))
        .expect("creates stream");
    let _stream = evt_loop
        .block_on(stream.send(Bytes::from(send_data)))
        .unwrap();

    let (new_stream, _con) = evt_loop
        .block_on(
            con.into_future()
                .map(|(m, c)| (m.unwrap(), c))
                .map_err(|(e, _)| e),
        )
        .unwrap();

    assert_eq!(
        send_data,
        String::from_utf8(
            evt_loop
                .block_on(new_stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
                .unwrap()
                .unwrap()
                .to_vec()
        )
        .unwrap()
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
) -> Result<(), Error> {
    let send_data = "hello server";
    let call_counter = VerifyCertificateImpl::new();

    let mut client_config = get_test_config();
    client_config.set_verify_certificate_handler(call_counter.clone());
    client_config.set_certificate_chain_filename(client_cert);
    client_config.set_private_key_filename(client_key);

    let (mut context, mut evt_loop) = create_context_and_evt_loop(client_config);

    let server_call_counter = call_counter.clone();
    let addr = start_server_that_sends_received_data_back(evt_loop.executor(), move || {
        let mut server_config = get_test_config();
        server_config.set_verify_certificate_handler(server_call_counter);
        server_config.enable_client_authentication();

        server_config
    });

    let mut con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))?;

    let stream = evt_loop.block_on(con.new_bidirectional_stream())?;
    let stream = evt_loop.block_on(stream.send(Bytes::from(send_data)))?;

    assert_eq!(
        send_data,
        &String::from_utf8(
            evt_loop
                .block_on(stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))?
                .unwrap()
                .to_vec()
        )
        .unwrap()
    );

    assert_eq!(call_counter.get(), 2);
    Ok(())
}

#[test]
fn verify_certificate_callback_is_called_and_certificate_verification_succeeds() {
    assert!(
        verify_certificate_callback_is_called_and_certificate_is_verified(
            format!("{}device.test.crt", get_test_certs_path()),
            format!("{}device.key", get_test_certs_path()),
        )
        .is_ok()
    );
}

#[test]
fn verify_certificate_callback_is_called_and_certificate_verification_fails() {
    assert_eq!(
        verify_certificate_callback_is_called_and_certificate_is_verified(
            format!("{}device.invalid.crt", get_test_certs_path()),
            format!("{}device.key", get_test_certs_path()),
        )
        .err()
        .unwrap()
        .kind(),
        &ErrorKind::TLSHandshakeError
    );
}

#[test]
fn set_certificate_and_key_from_memory() {
    client_connects_creates_bidirectional_stream_and_sends_data_impl(get_test_config(), || {
        let cert = include_bytes!("certs/device.test.crt");
        let key = include_bytes!("certs/device.key");

        let mut config = get_test_config();
        config.certificate_chain_filename = None;
        config.private_key_filename = None;
        config.set_certificate_chain(vec![cert.to_vec()], FileFormat::PEM);
        config.set_private_key(key.to_vec(), FileFormat::PEM);
        config
    });
}

#[test]
fn stream_stops_on_context_drop() {
    timebomb::timeout_ms(stream_stops_on_context_drop_inner, 10000);
}

fn stream_stops_on_context_drop_inner() {
    let send_data = "hello server";

    let (mut context, mut evt_loop) = create_context_and_evt_loop_with_default_config();

    let addr = start_server_that_sends_received_data_back(evt_loop.executor(), get_test_config);

    let mut con = evt_loop
        .block_on(context.new_connection(([127, 0, 0, 1], addr.port()).into(), TEST_SERVER_NAME))
        .expect("creates connection");

    let stream = evt_loop
        .block_on(
            con.new_bidirectional_stream()
                .and_then(move |s| s.send(Bytes::from(send_data))),
        )
        .expect("creates stream");

    evt_loop
        .block_on(
            stream
                .send_all(
                    Interval::new(Instant::now(), Duration::from_millis(10))
                        .map_err(|_| Error::from(ErrorKind::Unknown))
                        .map(move |_| Bytes::from(send_data)),
                )
                .map(|_| ())
                .select(
                    Delay::new(Instant::now() + Duration::from_millis(100))
                        .map_err(|_| ErrorKind::Unknown.into())
                        .map(move |_| {
                            let _ = context;
                        }),
                ),
        )
        .map_err(|e| e.0)
        .unwrap();

    assert!(evt_loop
        .block_on(con.into_future().map_err(|(e, _)| e).map(|v| v.0))
        .unwrap()
        .is_none());
}
