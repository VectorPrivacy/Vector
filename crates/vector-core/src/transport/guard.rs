//! A socket that dies with the decision that opened it: the next read or write after the epoch
//! moves, or after its session stops being live, fails. A switch closes every relay socket of
//! every client without a registry.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

type Changed = Pin<Box<dyn Future<Output = ()> + Send>>;

pub struct Guarded<S> {
    inner: S,
    owner: u64,
    changed: Changed,
    tripped: bool,
}

impl<S> Guarded<S> {
    pub fn new(inner: S, epoch: u64, owner: u64) -> Self {
        Guarded { inner, owner, changed: Box::pin(super::changed(epoch)), tripped: false }
    }

    /// Polling `changed` registers the waker, so an idle socket wakes and fails at the change.
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if !self.tripped
            && (self.changed.as_mut().poll(cx).is_ready() || crate::db::live_session_id() != self.owner)
        {
            self.tripped = true;
        }
        if self.tripped {
            Err(io::Error::new(io::ErrorKind::ConnectionAborted, "network changed"))
        } else {
            Ok(())
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Guarded<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if let Err(e) = self.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Guarded<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        if let Err(e) = self.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Err(e) = self.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// An upload body that ends with an error when the epoch or the live session changes.
pub fn guard_body<S, E>(
    body: S,
    epoch: u64,
    owner: u64,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, io::Error>> + Send + 'static
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    use futures_util::StreamExt;
    let changed: Changed = Box::pin(super::changed(epoch));
    futures_util::stream::unfold((Box::pin(body), changed, false), move |(mut body, mut changed, done)| async move {
        if done {
            return None;
        }
        let next = futures_util::future::select(body.next(), changed.as_mut()).await;
        match next {
            futures_util::future::Either::Left((Some(item), _)) => {
                if super::epoch() != epoch || crate::db::live_session_id() != owner {
                    let e = io::Error::new(io::ErrorKind::ConnectionAborted, "network changed");
                    return Some((Err(e), (body, changed, true)));
                }
                Some((item.map_err(|e| io::Error::other(e.to_string())), (body, changed, false)))
            }
            futures_util::future::Either::Left((None, _)) => None,
            futures_util::future::Either::Right(((), _)) => {
                let e = io::Error::new(io::ErrorKind::ConnectionAborted, "network changed");
                Some((Err(e), (body, changed, true)))
            }
        }
    })
}
