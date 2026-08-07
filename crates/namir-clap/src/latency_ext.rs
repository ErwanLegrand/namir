//! CLAP's `latency` extension (FR-CLAP-040's "report total latency in samples"). The
//! notify-on-change half — the interesting, order-sensitive part — lives in `crate::audio` (the
//! writer) and `crate::main_thread` (`on_main_thread`'s reaction); this module is only the
//! `get()` the host calls to read the value, which is a plain atomic load.

use clack_extensions::latency::PluginLatencyImpl;
use std::sync::atomic::Ordering;

use crate::main_thread::NamirMainThread;

impl<'a> PluginLatencyImpl for NamirMainThread<'a> {
    fn get(&mut self) -> u32 {
        self.shared.inner.latency_samples.load(Ordering::Relaxed)
    }
}
