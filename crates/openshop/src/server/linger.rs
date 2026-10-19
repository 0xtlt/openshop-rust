//! Connections closed without a reset under a client that is still sending.

use std::future::poll_fn;
use std::io::{self, IoSlice};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

/// How long a connection is still read after the server is done with it.
const LINGER: Duration = Duration::from_secs(5);

/// A listener whose connections are [`Lingering`].
pub(crate) struct LingeringListener(pub(crate) TcpListener);

impl Listener for LingeringListener {
    type Io = Lingering;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, addr) = Listener::accept(&mut self.0).await;
        let connection = Lingering {
            stream: Some(stream),
            sending_closed: false,
        };
        (connection, addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

/// The address of the peer of a request, read as `ConnectInfo<Peer>`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Peer(pub(crate) SocketAddr);

impl Connected<IncomingStream<'_, LingeringListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, LingeringListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

/// A connection that is closed in two steps.
///
/// A response can leave before its request has arrived in full: a body over
/// the limit is refused as soon as the limit is crossed. Closing the socket
/// then, with data unread or still on its way, makes the kernel reset the
/// connection, and a client that was still sending fails on its write and
/// never reads the answer. So only the sending side is closed at first, and
/// what the client sends is read and dropped until it closes in turn.
pub(crate) struct Lingering {
    stream: Option<TcpStream>,
    /// Whether hyper has closed the sending side, as it does for a connection it finished.
    sending_closed: bool,
}

impl Lingering {
    fn stream(self: Pin<&mut Self>) -> Pin<&mut TcpStream> {
        let stream = self.get_mut().stream.as_mut();
        Pin::new(stream.expect("the stream is only taken when the connection is dropped"))
    }
}

impl AsyncRead for Lingering {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        self.stream().poll_read(cx, buf)
    }
}

impl AsyncWrite for Lingering {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        self.stream().poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.stream().poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.as_ref().is_some_and(TcpStream::is_write_vectored)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let closed = self.as_mut().stream().poll_shutdown(cx);
        if matches!(closed, Poll::Ready(Ok(()))) {
            self.sending_closed = true;
        }
        closed
    }
}

impl Drop for Lingering {
    fn drop(&mut self) {
        let Some(stream) = self.stream.take() else { return };
        match stream.try_read(&mut [0]) {
            // The client has closed, or the connection is broken: nothing is on its way.
            Ok(0) => return,
            Err(error) if error.kind() != io::ErrorKind::WouldBlock => return,
            _ => {}
        }
        // Outside a runtime, when the server is torn down, the socket is closed as it is.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(linger(stream, self.sending_closed));
        }
    }
}

/// Closes the sending side, then reads until the client closes or [`LINGER`] has passed.
async fn linger(mut stream: TcpStream, sending_closed: bool) {
    let _ = tokio::time::timeout(LINGER, async {
        if !sending_closed {
            let _ = poll_fn(|cx| Pin::new(&mut stream).poll_shutdown(cx)).await;
        }
        // Most clients have nothing left to send and close: only the others get a buffer.
        if read_some(&stream, &mut [0; 64]).await == 0 {
            return;
        }
        let mut dropped = vec![0; 16 * 1024];
        while read_some(&stream, &mut dropped).await > 0 {}
    })
    .await;
}

/// Reads what has arrived: nothing once the client has closed, or on a broken connection.
async fn read_some(stream: &TcpStream, buffer: &mut [u8]) -> usize {
    loop {
        if stream.readable().await.is_err() {
            return 0;
        }
        match stream.try_read(buffer) {
            Ok(read) => return read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return 0,
        }
    }
}
