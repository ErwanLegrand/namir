//! D-9.5: verifies `PartitionedConvolver` against the `direct_convolve` reference, to a stated
//! numerical tolerance, "for every IR length and block size in the test matrix". Exits non-zero
//! on any failure so this can gate CI later, though as a spike it's a manual reproduction step.
//!
//! Fixture set follows D-9.5's own list: delta, delayed delta, decaying noise (D-19.1's
//! "designed minimum-phase filters" is left for the product test suite — no minimum-phase
//! design is needed to validate the *partitioning arithmetic*, which is what this spike is
//! actually de-risking).

use s2_ir_convolution::{PartitionedConvolver, direct_convolve, fixtures, rms_error_db};

// Numerical tolerance: f32 FFT round-trip error, not a structural discrepancy. -100 dB is
// comfortably below the -131 dB S-1 measured for a much larger f32 pipeline (NAM inference) and
// comfortably above the noise floor a single realfft round-trip actually produces.
const TOLERANCE_DB: f64 = -100.0;

fn ir_for(label: &str, ir_len: usize) -> Vec<f32> {
    match label {
        l if l.starts_with("delta") => fixtures::delta(ir_len),
        l if l.starts_with("delayed_delta") => fixtures::delayed_delta(ir_len, ir_len / 3),
        _ => fixtures::decaying_noise(ir_len, 0xD00D_D00D, ir_len as f64 / 6.0),
    }
}

fn main() {
    let block_sizes = [32usize, 64, 128, 256, 2048];
    let ir_lens = [1usize, 17, 63, 64, 65, 500, 4001, 12_000];
    // (growth_factor, max_partition_floor) — max_partition is clamped up to at least
    // block_size below, since build_schedule requires max_partition >= block_size.
    let schedules: [(usize, usize); 4] = [
        (1, 1), // uniform: growth_factor == 1 degenerates regardless of max_partition
        (2, 256),
        (3, 1024),
        (4, 512),
    ];
    let kinds = ["delta", "delayed_delta", "decaying_noise"];

    let mut total = 0usize;
    let mut failed = 0usize;
    let mut worst_db = f64::NEG_INFINITY;

    for &kind in &kinds {
        for &ir_len in &ir_lens {
            let h = ir_for(kind, ir_len);
            let direct = {
                let x = fixtures::white_noise(ir_len + 4000, 42);
                (direct_convolve(&h, &x), x)
            };
            let (reference, x) = direct;

            for &block_size in &block_sizes {
                for &(growth_factor, max_partition_floor) in &schedules {
                    let max_partition = max_partition_floor.max(block_size);
                    total += 1;

                    let mut conv =
                        PartitionedConvolver::new(&h, block_size, growth_factor, max_partition);
                    let mut y = vec![0f32; x.len()];
                    let mut i = 0;
                    while i < x.len() {
                        let end = (i + block_size).min(x.len());
                        conv.process_block(&x[i..end], &mut y[i..end]);
                        i = end;
                    }

                    match rms_error_db(&reference, &y) {
                        None => {} // silent reference (only the all-zero-length edge case)
                        Some(db) => {
                            worst_db = worst_db.max(db);
                            if db > TOLERANCE_DB {
                                failed += 1;
                                println!(
                                    "FAIL {kind:16} ir_len={ir_len:6} block={block_size:5} g={growth_factor} max_part={max_partition:6} error={db:.2} dB (limit {TOLERANCE_DB} dB)"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    println!();
    println!("=== D-9.5 verification: {total} cases, {failed} failed, worst error {worst_db:.2} dB (limit {TOLERANCE_DB} dB) ===");
    if failed > 0 {
        std::process::exit(1);
    }
}
