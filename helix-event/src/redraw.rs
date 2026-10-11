//! Signals that control when/if the editor redraws

use std::future::Future;

use parking_lot::{RwLock, RwLockReadGuard};
use tokio::sync::Notify;

use crate::runtime_local;

runtime_local! {
    /// A `Notify` instance that can be used to (asynchronously) request
    /// the editor to render a new frame.
    static REDRAW_NOTIFY: Notify = Notify::const_new();

    /// A `RwLock` that prevents the next frame from being
    /// drawn until an exclusive (write) lock can be acquired.
    /// This allows asynchronous tasks to acquire `non-exclusive`
    /// locks (read) to prevent the next frame from being drawn
    /// until a certain computation has finished.
    static RENDER_LOCK: RwLock<()> = RwLock::new(());
}

pub type RenderLockGuard = RwLockReadGuard<'static, ()>;

/// Requests that the editor is redrawn. The redraws are debounced (currently to
/// 30FPS) so this can be called many times without causing a ton of frames to
/// be rendered.
pub fn request_redraw() {
    REDRAW_NOTIFY.notify_one();
}

/// Capture the owning editor's redraw signal before handing it to a native
/// worker thread. In integration tests those threads do not enter Tokio, so
/// resolving a runtime-local signal inside the callback would panic.
pub fn redraw_callback() -> impl Fn() + Send + Sync + 'static {
    let notify: &'static Notify = &REDRAW_NOTIFY;
    move || notify.notify_one()
}

/// Returns a future that will yield once a redraw has been asynchronously
/// requested using [`request_redraw`].
pub fn redraw_requested() -> impl Future<Output = ()> {
    REDRAW_NOTIFY.notified()
}

/// Wait until all locks acquired with [`lock_frame`] have been released.
/// This function is called before rendering and is intended to allow the frame
/// to wait for async computations that should be included in the current frame.
pub fn start_frame() {
    drop(RENDER_LOCK.write());
    // exhaust any leftover redraw notifications
    let notify = REDRAW_NOTIFY.notified();
    tokio::pin!(notify);
    notify.enable();
}

/// Acquires the render lock which will prevent the next frame from being drawn
/// until the returned guard is dropped.
pub fn lock_frame() -> RenderLockGuard {
    RENDER_LOCK.read()
}

/// Requests a redraw of the owning editor when dropped, even on native threads.
#[derive(Clone)]
pub struct RequestRedrawOnDrop {
    notify: &'static Notify,
}

impl Default for RequestRedrawOnDrop {
    fn default() -> Self {
        Self {
            notify: &REDRAW_NOTIFY,
        }
    }
}

impl Drop for RequestRedrawOnDrop {
    fn drop(&mut self) {
        self.notify.notify_one();
    }
}
