pub mod frame;
pub mod quic;
pub mod tls;
pub mod transport;

#[cfg(feature = "recorder")]
pub mod recorder_hook {
    use origin_replay::ipc_tap::IpcTap;
    use std::sync::Arc;

    static TAP: parking_lot::RwLock<Option<Arc<IpcTap>>> = parking_lot::RwLock::new(None);

    pub fn register_tap(tap: Arc<IpcTap>) {
        *TAP.write() = Some(tap);
    }

    #[must_use]
    pub fn tap() -> Option<Arc<IpcTap>> {
        TAP.read().clone()
    }
}
