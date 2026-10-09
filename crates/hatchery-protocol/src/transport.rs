//! The one place in the workspace that knows what a local socket is made of (ADR-0013).
//!
//! tokio gates `net::unix` behind `cfg(unix)`, and the workspace's Windows target is
//! `x86_64-pc-windows-gnu` — a native Windows binary built inside MSYS2 UCRT64, which is *not*
//! `cfg(unix)`, so there is no AF_UNIX there at all. The daemon's socket is therefore a UDS on
//! unix and a named pipe on Windows, and everything above this module sees only the two halves
//! below: [`ReadHalf`] and [`WriteHalf`].
//!
//! An endpoint is a **locator**: a filesystem path on unix, and on Windows the string a pipe name
//! is derived from. It is what `daemon.json` publishes and what a client hands back to
//! [`connect`], so no caller has to know which of the two it is holding.
//!
//! Measured on `x86_64-pc-windows-gnu`, not read off a doc page:
//!
//! * a connect to a name nobody listens on fails fast with [`NotFound`](io::ErrorKind::NotFound)
//!   (measured at 56 µs), the same shape as a refused unix connect — so "can I connect?" is a
//!   portable liveness probe, which is how `TestDaemon` decides the daemon is up;
//! * **dropping one half does not end the connection.** The listener sees EOF only once both the
//!   read and the write half of the client are gone — and the client cannot count on dropping its
//!   writer to get there, because the router task is parked on a *quiet* read and holds the other
//!   half. Both routers therefore watch a cancellation token that their owner cancels as it dies;
//!   that is what lets the parked read half go, and on unix it costs nothing;
//! * binding a name a live listener already holds is refused, with
//!   [`PermissionDenied`](io::ErrorKind::PermissionDenied) rather than the unix
//!   [`AddrInUse`](io::ErrorKind::AddrInUse), because the refusal comes from the pipe's DACL;
//! * `D:P(A;;GA;;;ME)` denies **its own owner**: Windows does not resolve `ME` inside a pipe
//!   DACL. `D:P(A;;GA;;;OW)` is the owner-only form that measurably works — and it is what stands
//!   in for the 0700 a unix socket gets from `discover::restrict_to_owner`.
//!
//! The unix half deliberately stays tokio's own UDS: it is the transport every milestone since M1
//! has run on, and swapping it for the crate that fixes Windows would trade a working socket for
//! a uniform-looking one.

use std::io;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncWrite};

/// The readable half of a connection, boxed because its type names a platform socket.
pub type ReadHalf = Box<dyn AsyncRead + Send + Unpin>;

/// The writable half of a connection.
pub type WriteHalf = Box<dyn AsyncWrite + Send + Unpin>;

/// A listener on an endpoint.
pub struct Listener {
    inner: platform::Listener,
}

impl Listener {
    /// Waits for the next connection and splits it into its two halves.
    ///
    /// # Errors
    ///
    /// The OS's complaint about the pending connection. The daemon's accept loop logs one and
    /// keeps going, which is the right answer to a client that connects and hangs up.
    pub async fn accept(&self) -> io::Result<(ReadHalf, WriteHalf)> {
        platform::accept(&self.inner).await
    }
}

/// Connects to a daemon's endpoint.
///
/// # Errors
///
/// [`NotFound`](io::ErrorKind::NotFound) on Windows,
/// [`ConnectionRefused`](io::ErrorKind::ConnectionRefused) or
/// [`NotFound`](io::ErrorKind::NotFound) on unix when nobody is listening; any other OS failure.
/// Both shapes are what lets `attach_or_spawn` decide to spawn a daemon.
pub async fn connect(endpoint: &Path) -> io::Result<(ReadHalf, WriteHalf)> {
    platform::connect(endpoint).await
}

/// Listens on an endpoint.
///
/// On unix a socket file a killed daemon left behind is unlinked first — the behavior the server
/// had before this module existed.
///
/// # Errors
///
/// [`PermissionDenied`](io::ErrorKind::PermissionDenied) on Windows when a live listener holds the
/// name, [`InvalidInput`](io::ErrorKind::InvalidInput) when the locator is too deep to name a
/// pipe, and whatever the OS says about the bind itself.
pub fn bind(endpoint: &Path) -> io::Result<Listener> {
    platform::bind(endpoint).map(|inner| Listener { inner })
}

/// Removes the endpoint after a shutdown. A no-op on Windows: a named pipe is not a file, and it
/// goes away with its last handle.
///
/// # Errors
///
/// The OS's complaint about the unlink; [`NotFound`](io::ErrorKind::NotFound) is folded into
/// `Ok` because the goal was reached.
pub fn discard(endpoint: &Path) -> io::Result<()> {
    platform::discard(endpoint)
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::path::Path;

    use tokio::net::{UnixListener, UnixStream};

    use super::{ReadHalf, WriteHalf};

    pub struct Listener(UnixListener);

    fn split(stream: UnixStream) -> (ReadHalf, WriteHalf) {
        let (read, write) = stream.into_split();
        (Box::new(read), Box::new(write))
    }

    pub fn bind(endpoint: &Path) -> io::Result<Listener> {
        // A corpse socket file (its daemon was killed) cannot be bound over. The instance lock is
        // what makes a *live* second daemon impossible, so this unlink is not a race we can lose.
        let _ = std::fs::remove_file(endpoint);
        UnixListener::bind(endpoint).map(Listener)
    }

    pub async fn connect(endpoint: &Path) -> io::Result<(ReadHalf, WriteHalf)> {
        UnixStream::connect(endpoint).await.map(split)
    }

    pub async fn accept(listener: &Listener) -> io::Result<(ReadHalf, WriteHalf)> {
        listener
            .0
            .accept()
            .await
            .map(|(stream, _addr)| split(stream))
    }

    pub fn discard(endpoint: &Path) -> io::Result<()> {
        match std::fs::remove_file(endpoint) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::path::Path;

    use interprocess::local_socket::tokio::{Listener as Pipe, Stream};
    use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, ToNsName};
    use interprocess::os::windows::ToWtf16;
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;

    use super::{ReadHalf, WriteHalf};

    /// Everything before the name in `\\.\pipe\<name>`.
    pub(super) const PIPE_PREFIX_LEN: usize = r"\\.\pipe\".len();
    /// Windows caps a full pipe path at 256 characters, the terminator included.
    pub(super) const PIPE_NAME_LIMIT: usize = 255;
    /// Every hatchery pipe starts with this, so `Get-ChildItem \\.\pipe\` says whose it is.
    const PIPE_PREFIX: &str = "hatchery-";

    /// Owner-only, in the one SDDL form that measurably works on a pipe DACL. `P` protects the DACL
    /// from inheritance; `OW` is the pipe's owner — which `ME` is not, see the module docs.
    const OWNER_ONLY_SDDL: &str = "D:P(A;;GA;;;OW)";

    pub struct Listener(Pipe);

    /// The name a locator maps to: [`PIPE_PREFIX`] plus the locator with everything a pipe name may
    /// not carry percent-escaped — a pipe name may not contain a backslash. The mapping is
    /// deterministic, so the daemon that publishes a locator and the client that reads it back
    /// derive the same name, and a `--state-dir` override keeps two daemons' endpoints apart.
    pub(super) fn escaped_locator(locator: &Path) -> String {
        let raw = locator.to_string_lossy();
        let mut escaped = String::with_capacity(PIPE_PREFIX.len() + raw.len() + 8);
        escaped.push_str(PIPE_PREFIX);
        for byte in raw.as_bytes() {
            let character = *byte as char;
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                escaped.push(character);
            } else {
                escaped.push_str(&format!("%{byte:02X}"));
            }
        }
        escaped
    }

    pub(super) fn pipe_name(locator: &Path) -> io::Result<Name<'static>> {
        let escaped = escaped_locator(locator);
        if PIPE_PREFIX_LEN + escaped.len() > PIPE_NAME_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} would be {} characters as a named pipe, over the {} one allows",
                    locator.display(),
                    PIPE_PREFIX_LEN + escaped.len(),
                    PIPE_NAME_LIMIT
                ),
            ));
        }
        escaped.to_ns_name::<GenericNamespaced>()
    }

    pub fn bind(endpoint: &Path) -> io::Result<Listener> {
        let encoded = OWNER_ONLY_SDDL
            .to_wtf_16()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
        let descriptor = SecurityDescriptor::deserialize(&encoded)?;
        // `reclaim_name(false)`: the teardown disposer owns the endpoint's removal, in the order
        // the startup audit documents. Where there is a file to unlink, a listener that unlinked
        // itself on drop would race that order.
        ListenerOptions::new()
            .name(pipe_name(endpoint)?)
            .security_descriptor(descriptor)
            .reclaim_name(false)
            .create_tokio()
            .map(Listener)
    }

    fn split(stream: Stream) -> (ReadHalf, WriteHalf) {
        let (read, write) = stream.split();
        (Box::new(read), Box::new(write))
    }

    pub async fn connect(endpoint: &Path) -> io::Result<(ReadHalf, WriteHalf)> {
        Stream::connect(pipe_name(endpoint)?).await.map(split)
    }

    pub async fn accept(listener: &Listener) -> io::Result<(ReadHalf, WriteHalf)> {
        listener.0.accept().await.map(split)
    }

    pub fn discard(_endpoint: &Path) -> io::Result<()> {
        // No file to unlink: the pipe is gone when the listener and every connection to it die.
        Ok(())
    }
}

/// A platform with neither of the two transports gets a refusal that names itself, rather than a
/// `cfg(unix)` hole the build would have to trip over later.
#[cfg(not(any(unix, windows)))]
mod platform {
    use std::io;
    use std::path::Path;

    use super::{ReadHalf, WriteHalf};

    pub struct Listener;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "hatchery has no local socket transport on this platform (ADR-0013)",
        )
    }

    pub fn bind(_endpoint: &Path) -> io::Result<Listener> {
        Err(unsupported())
    }

    pub async fn connect(_endpoint: &Path) -> io::Result<(ReadHalf, WriteHalf)> {
        Err(unsupported())
    }

    pub async fn accept(_listener: &Listener) -> io::Result<(ReadHalf, WriteHalf)> {
        Err(unsupported())
    }

    pub fn discard(_endpoint: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn locator(dir: &Path, name: &str) -> std::path::PathBuf {
        dir.join(name)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_connection_carries_bytes_both_ways() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = locator(dir.path(), "roundtrip.sock");
        let listener = bind(&endpoint).expect("bind");

        let server = tokio::spawn(async move {
            let (mut read, mut write) = listener.accept().await.expect("accept");
            let mut frame = [0_u8; 4];
            read.read_exact(&mut frame).await.expect("read");
            assert_eq!(&frame, b"ping");
            write.write_all(b"pong").await.expect("write");
            write.flush().await.expect("flush");
        });

        let (mut read, mut write) = connect(&endpoint).await.expect("connect");
        write.write_all(b"ping").await.expect("write");
        write.flush().await.expect("flush");
        let mut reply = [0_u8; 4];
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read.read_exact(&mut reply),
        )
        .await
        .expect("a reply in time")
        .expect("read");
        assert_eq!(&reply, b"pong");
        server.await.expect("the server task");
    }

    /// The claim `attach_or_spawn` and `TestDaemon`'s readiness poll both lean on: an endpoint
    /// nobody listens on is an error, never a hang.
    #[tokio::test(flavor = "multi_thread")]
    async fn connecting_where_nobody_listens_refuses_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = locator(dir.path(), "absent.sock");
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(10), connect(&endpoint)).await;
        let error = match outcome {
            Err(_) => panic!("a connect to an absent endpoint hung"),
            Ok(Err(error)) => error,
            Ok(Ok(_)) => panic!("a connect to an absent endpoint succeeded"),
        };
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ),
            "unexpected kind: {error:?}"
        );
    }

    /// A listener sees a dead client only once *both* halves are gone (measured on Windows), which
    /// is why the writer lives in `Inner` and the router holds the read half — `DaemonClient`
    /// dropping is what closes the connection, and this pins that both halves do go.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_listener_sees_eof_once_the_client_drops_both_halves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = locator(dir.path(), "eof.sock");
        let listener = bind(&endpoint).expect("bind");
        let (read, write) = connect(&endpoint).await.expect("connect");
        drop(read);
        drop(write);

        let (mut server_read, _server_write) =
            tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                .await
                .expect("the connection is still acceptable")
                .expect("accept");
        let mut drained = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            server_read.read_to_end(&mut drained),
        )
        .await
        .expect("EOF, not a hang")
        .expect("read");
        assert!(drained.is_empty(), "unexpected bytes: {drained:?}");
    }

    /// What a second bind over one endpoint means. The two platforms disagree, deliberately and
    /// measurably: unix unlinks the corpse file a killed daemon left behind (the instance lock is
    /// what makes a live second daemon impossible), Windows refuses the name.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn unix_rebinds_over_a_socket_file_its_predecessor_left_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = locator(dir.path(), "corpse.sock");
        std::fs::write(&endpoint, b"not a socket, a corpse").expect("corpse file");
        let _listener = bind(&endpoint).expect("binding over a corpse socket file");
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn windows_refuses_an_endpoint_a_live_listener_already_holds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = locator(dir.path(), "held.sock");
        let _held = bind(&endpoint).expect("first bind");
        let error = match bind(&endpoint) {
            Ok(_) => panic!("the second bind must fail while the first listener lives"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error:?}");
    }

    #[test]
    fn discarding_an_endpoint_that_is_not_there_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = locator(dir.path(), "never-bound.sock");
        discard(&endpoint).expect("a missing endpoint is the goal reached");
    }

    /// The locator-to-name rule, pinned where it is testable without an OS: one deterministic,
    /// backslash-free name per locator, and a distinct one per state directory.
    #[cfg(windows)]
    #[test]
    fn a_locator_becomes_one_deterministic_flat_pipe_name() {
        let nested = Path::new(r"C:\Users\me\.local\state\hatchery\daemon\hatchery.sock");
        let other = Path::new(r"C:\Users\me\.local\state\hatchery\other\hatchery.sock");
        let name = super::platform::escaped_locator(nested);
        assert_eq!(name, super::platform::escaped_locator(nested));
        assert_ne!(name, super::platform::escaped_locator(other));
        assert!(name.starts_with("hatchery-"), "{name}");
        assert!(!name.contains('\\'), "{name}");
        assert!(
            super::platform::PIPE_PREFIX_LEN + name.len() <= super::platform::PIPE_NAME_LIMIT,
            "{name}"
        );
    }

    /// Measured: past the limit the OS answers `NotFound`, which reads like "nobody is listening"
    /// and would send the CLI off to spawn a second daemon. Refusing up front names the real fault.
    #[cfg(windows)]
    #[test]
    fn a_locator_too_deep_to_name_a_pipe_is_refused_before_the_os_says_so() {
        let deep = format!(
            r"C:\Users\me\{}\hatchery.sock",
            (0..40)
                .map(|index| format!("segment{index:02}"))
                .collect::<Vec<_>>()
                .join("\\")
        );
        let error = super::platform::pipe_name(Path::new(&deep)).expect_err("over the limit");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error:?}");
        assert!(error.to_string().contains("named pipe"), "{error}");
    }
}
