//! Read/write split for a stream whose halves are polled from different tasks.
//!
//! `tokio::io::split` forwards each half's own waker to the inner stream. That
//! is unsound for `yamux::Stream`: its `poll_read` (window updates) and
//! `poll_write` (data) park on one shared channel sender, which keeps a single
//! waker. A reader polling after a parked writer overwrites the writer's waker,
//! so the writer is never woken and the upload deadlocks. Here every inner poll
//! uses a waker that wakes *both* halves; a spurious wake costs one re-poll.

use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll, Wake, Waker},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub fn split<T: AsyncRead + AsyncWrite + Unpin>(io: T) -> (ReadHalf<T>, WriteHalf<T>) {
    let shared = Arc::new(Shared {
        io: Mutex::new(io),
        wakers: Arc::new(Wakers::default()),
    });
    (
        ReadHalf {
            shared: Arc::clone(&shared),
        },
        WriteHalf { shared },
    )
}

pub struct ReadHalf<T> {
    shared: Arc<Shared<T>>,
}

pub struct WriteHalf<T> {
    shared: Arc<Shared<T>>,
}

struct Shared<T> {
    io: Mutex<T>,
    wakers: Arc<Wakers>,
}

#[derive(Default)]
struct Wakers {
    read: Mutex<Option<Waker>>,
    write: Mutex<Option<Waker>>,
}

impl Wake for Wakers {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        for slot in [&self.read, &self.write] {
            if let Some(waker) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                waker.wake();
            }
        }
    }
}

impl<T: Unpin> Shared<T> {
    /// Registers `cx`'s waker in `slot`, then polls the inner stream with a
    /// waker that wakes both halves.
    fn poll_with<R>(
        &self,
        cx: &mut Context<'_>,
        slot: fn(&Wakers) -> &Mutex<Option<Waker>>,
        f: impl FnOnce(Pin<&mut T>, &mut Context<'_>) -> Poll<R>,
    ) -> Poll<R> {
        slot(&self.wakers)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(cx.waker().clone());
        let waker = Waker::from(Arc::clone(&self.wakers));
        let mut inner_cx = Context::from_waker(&waker);
        let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        f(Pin::new(&mut *io), &mut inner_cx)
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ReadHalf<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.shared
            .poll_with(cx, |w| &w.read, |io, cx| io.poll_read(cx, buf))
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteHalf<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.shared
            .poll_with(cx, |w| &w.write, |io, cx| io.poll_write(cx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shared
            .poll_with(cx, |w| &w.write, |io, cx| io.poll_flush(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shared
            .poll_with(cx, |w| &w.write, |io, cx| io.poll_shutdown(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Mimics `yamux::Stream`: reads and writes park on one waker slot, so the
    /// last poller wins and the other is never woken.
    #[derive(Default)]
    struct OneWakerIo {
        parked: Arc<Mutex<Option<Waker>>>,
        writable: Arc<AtomicBool>,
    }

    impl AsyncRead for OneWakerIo {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            *self.parked.lock().expect("test lock") = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    impl AsyncWrite for OneWakerIo {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.writable.load(Ordering::Acquire) {
                return Poll::Ready(Ok(buf.len()));
            }
            *self.parked.lock().expect("test lock") = Some(cx.waker().clone());
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Default)]
    struct Flag(AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn reader_poll_does_not_strand_a_parked_writer() {
        let io = OneWakerIo::default();
        let (parked, writable) = (Arc::clone(&io.parked), Arc::clone(&io.writable));
        let (mut reader, mut writer) = split(io);

        let writer_woken = Arc::new(Flag::default());
        let writer_waker = Waker::from(Arc::clone(&writer_woken));
        let mut writer_cx = Context::from_waker(&writer_waker);
        assert!(
            Pin::new(&mut writer)
                .poll_write(&mut writer_cx, b"x")
                .is_pending()
        );

        // The reader polls next and takes over the inner stream's only slot.
        let reader_waker = Waker::from(Arc::new(Flag::default()));
        let mut buf = [0u8; 1];
        let mut buf = ReadBuf::new(&mut buf);
        assert!(
            Pin::new(&mut reader)
                .poll_read(&mut Context::from_waker(&reader_waker), &mut buf)
                .is_pending()
        );

        // Capacity frees up: the inner stream wakes whatever it last stored.
        writable.store(true, Ordering::Release);
        parked
            .lock()
            .expect("test lock")
            .take()
            .expect("parked")
            .wake();
        assert!(writer_woken.0.load(Ordering::Acquire), "writer stranded");
        assert!(matches!(
            Pin::new(&mut writer).poll_write(&mut writer_cx, b"x"),
            Poll::Ready(Ok(1))
        ));
    }
}
