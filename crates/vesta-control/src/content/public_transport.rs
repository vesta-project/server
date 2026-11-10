//! Artifact-only transport: DNS answers become numeric dial targets. Check the
//! connected socket before TLS/HTTP writes; never delegate another name lookup.
use std::{
    fmt::Debug,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use ureq::unversioned::{
    resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
    transport::{Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport},
};
use vesta_server_install::public_address::{peer_matches, public_answers};

fn refused() -> ureq::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "public_artifact_destination_required",
    )
    .into()
}

#[derive(Debug, Default)]
pub(super) struct PublicResolver<R = DefaultResolver>(pub R);
impl<R: Resolver> Resolver for PublicResolver<R> {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addresses = self.0.resolve(uri, config, timeout)?;
        if !public_answers(&addresses) {
            return Err(refused());
        }
        Ok(addresses)
    }
}

trait SocketIo: Read + Write + Debug + Send + Sync + 'static {
    fn peer(&self) -> io::Result<SocketAddr>;
    fn open(&mut self) -> bool;
}
impl SocketIo for TcpStream {
    fn peer(&self) -> io::Result<SocketAddr> {
        self.peer_addr()
    }
    fn open(&mut self) -> bool {
        if self.set_nonblocking(true).is_err() {
            return false;
        }
        let mut byte = [0];
        let pending = self.read(&mut byte);
        let reset = self.set_nonblocking(false);
        reset.is_ok() && matches!(pending,Err(e) if e.kind()==io::ErrorKind::WouldBlock)
    }
}
trait Dial: Debug + Send + Sync + 'static {
    type Stream: SocketIo;
    fn dial(&self, address: SocketAddr, timeout: Duration) -> io::Result<Self::Stream>;
}
#[derive(Debug, Default)]
struct SystemDial;
#[cfg(test)]
pub(super) static LIVE_DIAL_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
impl Dial for SystemDial {
    type Stream = TcpStream;
    fn dial(&self, a: SocketAddr, t: Duration) -> io::Result<TcpStream> {
        #[cfg(test)]
        LIVE_DIAL_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        TcpStream::connect_timeout(&a, t)
    }
}

fn connect_checked<D: Dial>(
    dial: &D,
    addresses: &[SocketAddr],
    budget: Duration,
) -> Result<D::Stream, ureq::Error> {
    if !public_answers(addresses) || addresses.iter().any(|a| a.port() == 0) {
        return Err(refused());
    }
    let started = Instant::now();
    let mut last = None;
    for address in addresses {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        match dial.dial(*address, remaining) {
            Ok(stream) => {
                if !peer_matches(*address, stream.peer()?) {
                    return Err(refused());
                }
                return Ok(stream);
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "artifact_connect_timeout"))
        .into())
}

#[derive(Debug, Default)]
pub(super) struct PublicConnector;
impl Connector for PublicConnector {
    type Out = SocketTransport;
    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        if chained.is_some()
            || details.config.proxy().is_some()
            || details.uri.scheme_str() != Some("https")
        {
            return Err(refused());
        }
        let budget = if details.timeout.after.is_not_happening() {
            Duration::from_secs(10)
        } else {
            *details.timeout.after
        };
        let stream = connect_checked(&SystemDial, &details.addrs, budget)?;
        if details.config.no_delay() {
            stream.set_nodelay(true)?;
        }
        Ok(Some(SocketTransport {
            stream,
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
        }))
    }
}
#[derive(Debug)]
pub(super) struct SocketTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
}
fn timeout(next: NextTimeout) -> Result<Option<Duration>, ureq::Error> {
    if next.after.is_not_happening() {
        Ok(None)
    } else if next.after.is_zero() {
        Err(ureq::Error::Timeout(next.reason))
    } else {
        Ok(Some(*next.after))
    }
}
fn io_result<T>(result: io::Result<T>, next: NextTimeout) -> Result<T, ureq::Error> {
    result.map_err(|e| {
        if matches!(
            e.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            ureq::Error::Timeout(next.reason)
        } else {
            e.into()
        }
    })
}
impl Transport for SocketTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, next: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.set_write_timeout(timeout(next)?)?;
        io_result(
            self.stream.write_all(&self.buffers.output()[..amount]),
            next,
        )
    }
    fn await_input(&mut self, next: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.set_read_timeout(timeout(next)?)?;
        let count = io_result(self.stream.read(self.buffers.input_append_buf()), next)?;
        self.buffers.input_appended(count);
        Ok(count > 0)
    }
    fn is_open(&mut self) -> bool {
        self.stream
            .peer()
            .is_ok_and(vesta_server_install::public_address::is_public_socket)
            && self.stream.open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Debug)]
    struct Answers {
        values: Vec<SocketAddr>,
        calls: Arc<AtomicUsize>,
    }
    impl Resolver for Answers {
        fn resolve(
            &self,
            _: &ureq::http::Uri,
            _: &ureq::config::Config,
            _: NextTimeout,
        ) -> Result<ResolvedSocketAddrs, ureq::Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut out = self.empty();
            for address in &self.values {
                out.push(*address);
            }
            Ok(out)
        }
    }
    #[derive(Debug)]
    struct Stream {
        peer: SocketAddr,
        writes: Arc<AtomicUsize>,
    }
    impl Read for Stream {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl Write for Stream {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.writes.fetch_add(b.len(), Ordering::SeqCst);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl SocketIo for Stream {
        fn peer(&self) -> io::Result<SocketAddr> {
            Ok(self.peer)
        }
        fn open(&mut self) -> bool {
            true
        }
    }
    #[derive(Debug)]
    struct FakeDial {
        peer: SocketAddr,
        calls: Mutex<Vec<SocketAddr>>,
        writes: Arc<AtomicUsize>,
    }
    impl Dial for FakeDial {
        type Stream = Stream;
        fn dial(&self, a: SocketAddr, _: Duration) -> io::Result<Stream> {
            self.calls.lock().unwrap().push(a);
            Ok(Stream {
                peer: self.peer,
                writes: self.writes.clone(),
            })
        }
    }
    #[test]
    fn dns_results_are_pinned_once_and_peer_is_checked_before_any_writes() {
        let public = "8.8.8.8:443".parse().unwrap();
        let private = "127.0.0.1:443".parse().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = PublicResolver(Answers {
            values: vec![public],
            calls: calls.clone(),
        });
        let next = NextTimeout {
            after: Duration::from_secs(1).into(),
            reason: ureq::Timeout::Connect,
        };
        let addresses = resolver
            .resolve(
                &"https://artifact.example/a".parse().unwrap(),
                &ureq::Agent::config_builder().build(),
                next,
            )
            .unwrap();
        for peer in [public, private, "1.1.1.1:443".parse().unwrap()] {
            let writes = Arc::new(AtomicUsize::new(0));
            let dial = FakeDial {
                peer,
                calls: Mutex::new(vec![]),
                writes: writes.clone(),
            };
            assert_eq!(
                connect_checked(&dial, &addresses, Duration::from_secs(1)).is_ok(),
                peer == public
            );
            assert_eq!(*dial.calls.lock().unwrap(), vec![public]);
            assert_eq!(writes.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no second hostname resolution at connect"
        );
        for values in [vec![], vec![private], vec![public, private]] {
            let resolver = PublicResolver(Answers {
                values,
                calls: calls.clone(),
            });
            assert!(
                resolver
                    .resolve(
                        &"https://artifact.example/a".parse().unwrap(),
                        &ureq::Agent::config_builder().build(),
                        next
                    )
                    .is_err()
            );
        }
        let dial = FakeDial {
            peer: public,
            calls: Mutex::new(vec![]),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        assert!(connect_checked(&dial, &[public, private], Duration::from_secs(1)).is_err());
        assert!(dial.calls.lock().unwrap().is_empty());
    }
}
