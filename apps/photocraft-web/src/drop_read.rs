//! Capture a drop's destination before its asynchronous file read starts.

use std::future::Future;

/// Bind the current destination to a read, even if another document becomes active while it waits.
pub(crate) fn captured_read<T, F, R>(target: impl FnOnce() -> T, read: F) -> impl Future<Output = (T, R)>
where
    F: Future<Output = R>,
{
    let target = target();
    async move { (target, read.await) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        future::Future,
        pin::pin,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };

    struct Noop;
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }

    #[test]
    fn captures_each_document_before_delayed_reads_complete_in_reverse() {
        let active = Cell::new(1);
        let ready_a = Cell::new(false);
        let ready_b = Cell::new(false);
        let read_a = std::future::poll_fn(|_| if ready_a.get() { Poll::Ready("a") } else { Poll::Pending });
        let mut a = pin!(captured_read(|| active.get(), read_a));
        active.set(2);
        let read_b = std::future::poll_fn(|_| if ready_b.get() { Poll::Ready("b") } else { Poll::Pending });
        let mut b = pin!(captured_read(|| active.get(), read_b));
        let waker = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&waker);
        assert!(a.as_mut().poll(&mut cx).is_pending());
        assert!(b.as_mut().poll(&mut cx).is_pending());
        active.set(3);
        ready_b.set(true);
        assert_eq!(b.as_mut().poll(&mut cx), Poll::Ready((2, "b")));
        ready_a.set(true);
        assert_eq!(a.as_mut().poll(&mut cx), Poll::Ready((1, "a")));
    }
}
