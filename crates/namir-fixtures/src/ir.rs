//! D-9.5's convolution-correctness fixtures: delta, delayed delta, decaying noise (ported near
//! as-is from the S-2 spike's `fixtures` module), plus the designed minimum-phase filter D-9.5
//! lists that neither spike implemented.

/// A unit impulse: the simplest possible analytically-known IR. Convolving with it is the
/// identity, so any deviation in a convolution engine's output shows up as pure engine error.
pub fn delta(len: usize) -> Vec<f32> {
    let mut h = vec![0f32; len];
    if len > 0 {
        h[0] = 1.0;
    }
    h
}

/// An impulse delayed by `delay` samples — exercises non-zero-offset partitions (a partitioned
/// convolver's later, larger stages) that `delta` alone never touches.
pub fn delayed_delta(len: usize, delay: usize) -> Vec<f32> {
    let mut h = vec![0f32; len];
    if delay < len {
        h[delay] = 1.0;
    }
    h
}

/// Exponentially decaying white noise: the standard stand-in for a "realistic-shaped" IR. A
/// real cabinet/room IR's cost depends only on its length, not its exact taps, so this is
/// exercised for cost/coverage rather than tonal fidelity.
pub fn decaying_noise(len: usize, seed: u64, tau_samples: f64) -> Vec<f32> {
    use rand::Rng;
    use rand::SeedableRng;
    let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
    (0..len)
        .map(|i| {
            let env = (-(i as f64) / tau_samples).exp();
            (rng.gen_range(-1.0f64..1.0) * env) as f32
        })
        .collect()
}

/// The closed-form target for [`minimum_phase_lowpass`]: an `order`-th order Butterworth
/// magnitude response, |H(f)| = 1 / sqrt(1 + (f/cutoff_hz)^(2*order)). Even in `f`, so it needs
/// no special-casing for negative/mirrored frequencies when building a full FFT-length grid.
/// Exposed so the correctness test can compare the constructed filter against the exact same
/// formula the filter was designed from, rather than a hand-copied approximation of it.
pub fn butterworth_magnitude(freq_hz: f64, cutoff_hz: f64, order: u32) -> f64 {
    1.0 / (1.0 + (freq_hz / cutoff_hz).powi(2 * order as i32)).sqrt()
}

/// A designed minimum-phase lowpass filter — D-9.5's fourth convolution fixture kind, built via
/// the standard complex-cepstrum method (real cepstrum, folded to minimum phase, exponentiated
/// back): pick a target *magnitude* response, derive a *phase* for it that is causal and
/// minimum-energy-delay, and return the resulting impulse response. Analytically verifiable:
/// the FFT magnitude of the result should match `butterworth_magnitude` at any frequency.
///
/// `len` is the number of impulse-response taps returned. Internally this designs the filter on
/// a much larger FFT grid (`fft_len`, a power of two, at least `8 * len` and `4096`) than the
/// requested tap count and then truncates: a minimum-phase impulse response is theoretically
/// infinite, and a too-short cepstrum grid time-aliases the fold step in a way that visibly
/// distorts the passband. Truncating after computing on the larger grid, rather than designing
/// directly at `len`, is what keeps the result close to the closed-form target at `len` as small
/// as a few hundred taps.
pub fn minimum_phase_lowpass(len: usize, sample_rate: f64, cutoff_hz: f64, order: u32) -> Vec<f32> {
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex64;

    if len == 0 {
        return Vec::new();
    }
    let fft_len = (len.saturating_mul(8)).max(4096).next_power_of_two();

    // Step (b): log-magnitude over the full FFT-length grid, including the mirrored upper half.
    // A floor keeps `ln` finite deep in the stopband, where the true magnitude underflows f64
    // long before it reaches the mathematical zero it only approaches asymptotically.
    const MAG_FLOOR: f64 = 1e-8;
    let mut log_mag: Vec<Complex64> = (0..fft_len)
        .map(|k| {
            let bin = if k <= fft_len / 2 { k } else { fft_len - k };
            let f = bin as f64 * sample_rate / fft_len as f64;
            let mag = butterworth_magnitude(f, cutoff_hz, order).max(MAG_FLOOR);
            Complex64::new(mag.ln(), 0.0)
        })
        .collect();

    let mut planner = FftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(fft_len);
    let fft = planner.plan_fft_forward(fft_len);

    // Step (c): real cepstrum. rustfft's inverse is unnormalized (scale by 1/N); the imaginary
    // part is discarded rather than asserted-zero because a real, even input only produces an
    // exactly-real IFFT in exact arithmetic — float rounding leaves a residue at the 1e-15 level.
    ifft.process(&mut log_mag);
    let scale = 1.0 / fft_len as f64;
    let mut cepstrum: Vec<f64> = log_mag.iter().map(|c| c.re * scale).collect();

    // Step (d): fold to minimum phase. Indices 0 and (for even fft_len) the Nyquist bin N/2 are
    // each other's own mirror under c[n] <-> c[N-n], so they're left unscaled; everything
    // strictly between 0 and N/2 is doubled (folding the anti-causal energy from N/2+1..N-1
    // forward onto it); N/2+1..N-1 is zeroed, discarding the anti-causal half outright.
    let nyquist = fft_len / 2;
    for c in cepstrum.iter_mut().take(nyquist).skip(1) {
        *c *= 2.0;
    }
    for c in cepstrum.iter_mut().skip(nyquist + 1) {
        *c = 0.0;
    }

    // Steps (e)-(f): FFT back, exponentiate (complex exp turns the folded log-spectrum into a
    // minimum-phase complex spectrum with the *same* magnitude as the target but a causal
    // phase), then IFFT to the time-domain impulse response.
    let mut folded: Vec<Complex64> = cepstrum.iter().map(|&re| Complex64::new(re, 0.0)).collect();
    fft.process(&mut folded);
    for c in folded.iter_mut() {
        *c = c.exp();
    }
    ifft.process(&mut folded);
    let scale = 1.0 / fft_len as f64;

    folded
        .iter()
        .take(len)
        .map(|c| (c.re * scale) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_is_a_unit_impulse_at_zero() {
        let h = delta(8);
        assert_eq!(h[0], 1.0);
        assert!(h[1..].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn delta_of_zero_length_does_not_panic() {
        assert_eq!(delta(0), Vec::<f32>::new());
    }

    #[test]
    fn delayed_delta_places_impulse_at_delay() {
        let h = delayed_delta(10, 3);
        for (i, &v) in h.iter().enumerate() {
            if i == 3 {
                assert_eq!(v, 1.0);
            } else {
                assert_eq!(v, 0.0);
            }
        }
    }

    #[test]
    fn delayed_delta_with_out_of_range_delay_is_all_zero() {
        let h = delayed_delta(10, 20);
        assert!(h.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn decaying_noise_is_deterministic_for_a_given_seed() {
        let a = decaying_noise(256, 42, 100.0);
        let b = decaying_noise(256, 42, 100.0);
        assert_eq!(a, b);
    }

    #[test]
    fn decaying_noise_differs_across_seeds() {
        let a = decaying_noise(256, 1, 100.0);
        let b = decaying_noise(256, 2, 100.0);
        assert_ne!(a, b);
    }

    #[test]
    fn decaying_noise_envelope_actually_decays() {
        // Compare RMS of the first vs last quarter: the envelope should make the tail much
        // quieter on average, even though individual noise samples are random.
        let h = decaying_noise(4000, 7, 200.0);
        let rms = |s: &[f32]| (s.iter().map(|&v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let head_rms = rms(&h[..1000]);
        let tail_rms = rms(&h[3000..]);
        assert!(
            tail_rms < head_rms * 0.5,
            "expected decay: head_rms={head_rms}, tail_rms={tail_rms}"
        );
    }

    #[test]
    fn minimum_phase_lowpass_has_requested_length() {
        let h = minimum_phase_lowpass(512, 48_000.0, 2_000.0, 4);
        assert_eq!(h.len(), 512);
    }

    #[test]
    fn minimum_phase_lowpass_of_zero_length_does_not_panic() {
        assert_eq!(
            minimum_phase_lowpass(0, 48_000.0, 2_000.0, 4),
            Vec::<f32>::new()
        );
    }

    #[test]
    fn minimum_phase_lowpass_is_deterministic() {
        let a = minimum_phase_lowpass(512, 48_000.0, 2_000.0, 4);
        let b = minimum_phase_lowpass(512, 48_000.0, 2_000.0, 4);
        assert_eq!(a, b);
    }

    #[test]
    fn minimum_phase_lowpass_is_causal_and_energy_concentrated_near_zero() {
        // Not near-zero at t=0 (that would indicate a bad phase/normalization), and most of the
        // impulse response's energy should be in its front half — the defining behaviour of
        // "minimum phase" versus, say, a linear-phase (symmetric, non-causal-feeling) design.
        let h = minimum_phase_lowpass(2048, 48_000.0, 2_000.0, 4);
        assert!(h[0].abs() > 1e-4, "h[0] = {} is implausibly small", h[0]);
        let energy = |s: &[f32]| s.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>();
        let front = energy(&h[..h.len() / 4]);
        let back = energy(&h[3 * h.len() / 4..]);
        assert!(
            front > back * 10.0,
            "expected energy concentrated early: front={front}, back={back}"
        );
    }

    /// Measures |H(f)| of an impulse response at one frequency via a zero-padded real FFT.
    fn measured_magnitude(h: &[f32], sample_rate: f64, freq_hz: f64) -> f64 {
        use realfft::RealFftPlanner;
        let fft_len = (h.len() * 4).next_power_of_two().max(8192);
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(fft_len);
        let mut input = vec![0f32; fft_len];
        input[..h.len()].copy_from_slice(h);
        let mut spectrum = r2c.make_output_vec();
        r2c.process(&mut input, &mut spectrum).unwrap();
        let bin = (freq_hz / sample_rate * fft_len as f64).round() as usize;
        spectrum[bin.min(spectrum.len() - 1)].norm() as f64
    }

    #[test]
    fn minimum_phase_lowpass_magnitude_matches_the_analytic_target() {
        let sample_rate = 48_000.0;
        let cutoff = 2_000.0;
        let order = 4;
        let h = minimum_phase_lowpass(4096, sample_rate, cutoff, order);

        // Representative frequencies spanning passband, the -3 dB point, and stopband. The
        // tolerance is generous (this fixture only needs to be "analytically verifiable", not
        // bit-exact) but tight enough to catch a wrong fold step or a missing normalization.
        for &freq in &[100.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0] {
            let target = butterworth_magnitude(freq, cutoff, order);
            let measured = measured_magnitude(&h, sample_rate, freq);
            let err_db = 20.0 * (measured / target).log10();
            assert!(
                err_db.abs() < 1.5,
                "freq={freq}: target={target:.6}, measured={measured:.6}, err={err_db:.3} dB"
            );
        }
    }
}
