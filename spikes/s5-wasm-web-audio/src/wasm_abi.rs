//! Flat pointer/length ABI for the browser host. No wasm-bindgen: the interface
//! is narrow enough that generated glue buys nothing, and there is no `fetch`
//! inside an AudioWorkletGlobalScope anyway.
//!
//! Edition 2024 requires `unsafe extern` and `#[unsafe(no_mangle)]`. That is free
//! here because `spikes/` sits outside the workspace's `unsafe_code = "forbid"`.

use std::cell::{Cell, RefCell};

use crate::harness::{BLOCK_SIZE, Harness, Signal, Stats};

unsafe extern "C" {
    /// Supplied by the host as `env.now_us`. In a cross-origin-isolated Worker
    /// this is `performance.now() * 1000`, i.e. 5 microsecond resolution.
    fn now_us() -> f64;
}

thread_local! {
    static HARNESS: RefCell<Option<Harness>> = const { RefCell::new(None) };
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static STATS: RefCell<[f64; 5]> = const { RefCell::new([0.0; 5]) };
    static CENSUS: RefCell<[f64; 6]> = const { RefCell::new([0.0; 6]) };
    static IO: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    static RENDER: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    /// `process` call count since the last `init`, for the one-shot handover check.
    static PROCESSED: Cell<u32> = const { Cell::new(0) };
}

/// Which `process` call runs `assert_resources_loaded`. D-8.1's crossfade is
/// `HANDOVER_CROSSFADE_MS` = 20 ms = 960 frames = 7.5 blocks, so 256 blocks (682 ms) is
/// ~34x the margin needed -- late enough never to false-positive, early enough that a
/// worklet fed by an unloaded chain traps in the first second instead of quietly
/// emitting silence and reporting zero underruns.
const HANDOVER_GUARD_BLOCK: u32 = 256;

/// Returns a pointer to a buffer of `len` bytes for the host to write into.
/// One buffer at a time: the host must call `alloc` then consume it before the
/// next `alloc`.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    SCRATCH.with(|s| {
        let mut s = s.borrow_mut();
        s.clear();
        s.resize(len, 0);
        s.as_mut_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn init(sample_rate: u32) -> u32 {
    match Harness::new(sample_rate, BLOCK_SIZE) {
        Ok(h) => {
            HARNESS.with(|c| *c.borrow_mut() = Some(h));
            IO.with(|c| *c.borrow_mut() = vec![0.0; BLOCK_SIZE]);
            PROCESSED.with(|c| c.set(0));
            0
        }
        Err(_) => 1,
    }
}

fn with_scratch<F: FnOnce(&mut Harness, &[u8]) -> Result<(), String>>(len: usize, f: F) -> u32 {
    SCRATCH.with(|s| {
        let s = s.borrow();
        let bytes = &s[..len.min(s.len())];
        HARNESS.with(|c| match c.borrow_mut().as_mut() {
            Some(h) => match f(h, bytes) {
                Ok(()) => 0,
                Err(_) => 2,
            },
            None => 1,
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn load_nam(_ptr: *const u8, len: usize) -> u32 {
    with_scratch(len, |h, b| h.load_nam(b))
}

#[unsafe(no_mangle)]
pub extern "C" fn load_ir(_ptr: *const u8, len: usize) -> u32 {
    with_scratch(len, |h, b| h.load_ir(b))
}

/// `signal`: 0 steady, 1 amplitude-decay, 2 subnormal-tail. Was a `decaying: u32`
/// boolean until Task 5 added the third mode.
#[unsafe(no_mangle)]
pub extern "C" fn bench(warmup: u32, measured: u32, signal: u32) -> u32 {
    let signal = Signal::from_code(signal);
    let clock = || unsafe { now_us() };
    HARNESS.with(|c| match c.borrow_mut().as_mut() {
        Some(h) => {
            // `Harness::run` carries both guards itself: `assert_resources_loaded`
            // after warmup and `fault_count() == 0` after the measured loop.
            let s: Stats = h.run(warmup, measured, signal, &clock);
            STATS.with(|st| *st.borrow_mut() = [s.p50, s.p99, s.p999, s.max, s.estimator]);
            0
        }
        None => 1,
    })
}

/// Untimed census pass, same signal generator as `bench`. Writes six f64s to `STATS`'s
/// sibling buffer: blocks, subnormal-output blocks, subnormal-output samples, MXCSR-DE
/// blocks, MXCSR-UE blocks, smallest non-zero |output|. **The two MXCSR fields are
/// always 0 on wasm32** -- there is no such register, which is the entire premise of
/// this sub-experiment; the wasm witness is the output-side count.
#[unsafe(no_mangle)]
pub extern "C" fn census(warmup: u32, measured: u32, signal: u32) -> u32 {
    HARNESS.with(|c| match c.borrow_mut().as_mut() {
        Some(h) => {
            let c = h.census(warmup, measured, Signal::from_code(signal));
            CENSUS.with(|st| {
                *st.borrow_mut() = [
                    c.blocks as f64,
                    c.subnormal_output_blocks as f64,
                    c.subnormal_output_samples as f64,
                    c.denormal_flag_blocks as f64,
                    c.underflow_flag_blocks as f64,
                    c.min_abs_nonzero as f64,
                ]
            });
            0
        }
        None => 1,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn census_ptr() -> *const f64 {
    CENSUS.with(|s| s.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn stats_ptr() -> *const f64 {
    STATS.with(|s| s.borrow().as_ptr())
}

/// Renders the same deterministic input `native_bench` uses, so the host can
/// compare against `fixtures/reference_render_f32le.bin`. `Harness::render`
/// carries the loaded-resources and fault-count guards.
#[unsafe(no_mangle)]
pub extern "C" fn render(n: usize) -> *const f32 {
    let input: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
    RENDER.with(|r| {
        let mut r = r.borrow_mut();
        r.clear();
        r.resize(n, 0.0);
        HARNESS.with(|c| {
            if let Some(h) = c.borrow_mut().as_mut() {
                h.render(&input, &mut r);
            }
        });
        r.as_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn io_ptr() -> *mut f32 {
    IO.with(|c| c.borrow_mut().as_mut_ptr())
}

/// Processes one BLOCK_SIZE-frame block from `io_ptr` **in place**: the left output
/// channel is written back over the input. Without the write-back Task 6's worklet
/// would emit whatever it put in, i.e. a bypass, which is the same class of silent
/// failure the parity check exists to catch.
#[unsafe(no_mangle)]
pub extern "C" fn process() {
    IO.with(|io| {
        let mut io = io.borrow_mut();
        HARNESS.with(|c| {
            if let Some(h) = c.borrow_mut().as_mut() {
                h.process_block(&io);
                io.copy_from_slice(h.output_left());

                // `run` and `render` both carry these; without them here, Task 6's
                // worklet driven against an unloaded chain would emit silence quietly
                // and report zero underruns -- for exactly the wrong reason. Same
                // failure mode as the missing `io_ptr` write-back, one layer down.
                assert_eq!(
                    h.fault_count(),
                    0,
                    "process() hit FR-CHAIN-080's NaN/Inf fault path"
                );
                let n = PROCESSED.with(|p| {
                    let n = p.get().saturating_add(1);
                    p.set(n);
                    n
                });
                if n == HANDOVER_GUARD_BLOCK {
                    // Once, not per block: this drains the telemetry ring into a 2 KB
                    // stack buffer, which is not something to do every 128 frames on an
                    // audio thread.
                    h.assert_resources_loaded();
                }
            }
        });
    });
}
