//! Eq stage (FR-EQ-010/020/030): five `namir_dsp::Biquad` filters per channel — low shelf, mid
//! peaking, high shelf, plus a defeatable high-pass and low-pass ("as in FR-IR-070") — wired into
//! the `Stage` trait and FR-CHAIN-020's per-stage bypass.
//!
//! # Why this is per-channel, not mono-core
//!
//! Unlike Gate/Nam (FR-CHAIN-050's mono-core-then-duplicate stages), Eq processes every channel
//! independently: each channel gets its own five-`Biquad` cascade (own `s1`/`s2` state per
//! channel), all driven from the same shared coefficient *targets* (one set of parameters, per
//! FR-EQ-010 — there is no per-channel EQ control). This is both simpler and correct here because
//! a `Biquad` is a pure function of its own input stream; running one per channel costs nothing
//! extra in allocation (all sized once in `prepare`) and avoids the channel-0-then-duplicate
//! shuttle-buffer dance Gate/Nam need — there is no cross-channel copying in this stage at all.
//!
//! # FR-CHAIN-020 bypass without a dry/wet crossfade
//!
//! `Biquad::process` is always safe to call — it is the identity operator whenever its current
//! coefficients equal `BiquadCoeffs::identity()` (`biquad.rs`'s own doc comment) — and
//! `Biquad::set_coeffs` already interpolates any coefficient change across a ramp (D-9.9),
//! including a change *to* or *from* `identity()`. So unlike the shared dry/wet crossfade pattern
//! Gate/Nam/Ir use for FR-CHAIN-020 (this stage's own "wet" signal isn't expressible as a single
//! coefficient interpolation target for those stages), Eq's bypass — and the independent HP/LP
//! defeat toggles — reuse exactly one mechanism: smoothly interpolate the affected band(s) to
//! `identity()` when off, and to their designed coefficients when on.

use namir_dsp::{Biquad, BiquadCoeffs, FilterKind};
use namir_params::ParamKind;
use namir_params::stages::eq::{
    ENABLED, HIGH_PASS_ENABLED, HIGH_PASS_FREQ_HZ, HIGH_SHELF_FREQ_HZ, HIGH_SHELF_GAIN_DB,
    LOW_PASS_ENABLED, LOW_PASS_FREQ_HZ, LOW_SHELF_FREQ_HZ, LOW_SHELF_GAIN_DB, MID_FREQ_HZ,
    MID_GAIN_DB, MID_Q,
};

use namir_core::SampleRate;

use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::telemetry::TelemetrySink;

/// Fixed per-channel cascade order (this stage's own signal-path convention — the combined
/// magnitude response of a linear cascade doesn't depend on band order, so this is not itself an
/// FRS requirement): high-pass, low shelf, mid peaking, high shelf, low-pass.
const BAND_HIGH_PASS: usize = 0;
/// See [`BAND_HIGH_PASS`].
const BAND_LOW_SHELF: usize = 1;
/// See [`BAND_HIGH_PASS`].
const BAND_MID: usize = 2;
/// See [`BAND_HIGH_PASS`].
const BAND_HIGH_SHELF: usize = 3;
/// See [`BAND_HIGH_PASS`].
const BAND_LOW_PASS: usize = 4;
/// Number of bands in the per-channel cascade; the length of [`EqStage::biquads`]'s inner array.
const BAND_COUNT: usize = 5;

/// `BiquadCoeffs::design`'s `q` argument is algebraically unused for `FilterKind::LowShelf`/
/// `HighShelf` (that function's own `alpha_s` derivation fixes the shelf slope via a constant
/// `S = 1`, never `q` — see `biquad.rs`), so this is only a placeholder to satisfy the call
/// signature, not a control this stage exposes.
const SHELF_Q_UNUSED: f64 = 0.707;

/// Butterworth-flat (maximally-flat magnitude) `Q` for the defeatable high-pass/low-pass corners
/// (FR-EQ-010's "as in FR-IR-070"): neither has a dedicated `Q` parameter, so this is the RBJ
/// cookbook's own standard second-order default rather than an arbitrary pick.
const HIGH_PASS_LOW_PASS_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// This stage's RT-facing `namir_engine::ParamId`s, converted once from `namir_params`'s own ids
/// for the same keys (see `trim.rs`'s identical convention and its doc comment for why the two
/// crates carry distinct `ParamId` types on purpose).
const ENABLED_ID: ParamId = ParamId(ENABLED.id.0);
/// See [`ENABLED_ID`].
const LOW_SHELF_FREQ_HZ_ID: ParamId = ParamId(LOW_SHELF_FREQ_HZ.id.0);
/// See [`ENABLED_ID`].
const LOW_SHELF_GAIN_DB_ID: ParamId = ParamId(LOW_SHELF_GAIN_DB.id.0);
/// See [`ENABLED_ID`].
const MID_FREQ_HZ_ID: ParamId = ParamId(MID_FREQ_HZ.id.0);
/// See [`ENABLED_ID`].
const MID_GAIN_DB_ID: ParamId = ParamId(MID_GAIN_DB.id.0);
/// See [`ENABLED_ID`].
const MID_Q_ID: ParamId = ParamId(MID_Q.id.0);
/// See [`ENABLED_ID`].
const HIGH_SHELF_FREQ_HZ_ID: ParamId = ParamId(HIGH_SHELF_FREQ_HZ.id.0);
/// See [`ENABLED_ID`].
const HIGH_SHELF_GAIN_DB_ID: ParamId = ParamId(HIGH_SHELF_GAIN_DB.id.0);
/// See [`ENABLED_ID`].
const HIGH_PASS_ENABLED_ID: ParamId = ParamId(HIGH_PASS_ENABLED.id.0);
/// See [`ENABLED_ID`].
const HIGH_PASS_FREQ_HZ_ID: ParamId = ParamId(HIGH_PASS_FREQ_HZ.id.0);
/// See [`ENABLED_ID`].
const LOW_PASS_ENABLED_ID: ParamId = ParamId(LOW_PASS_ENABLED.id.0);
/// See [`ENABLED_ID`].
const LOW_PASS_FREQ_HZ_ID: ParamId = ParamId(LOW_PASS_FREQ_HZ.id.0);

/// Reads a `Continuous` descriptor's default, panicking (defensively; unreachable from any input
/// `prepare` is passed) if a future edit to `namir-params` changes the descriptor's `kind` out
/// from under this file. Matches `gate.rs`'s identical helper.
fn continuous_default(descriptor: namir_params::ParamDescriptor) -> f32 {
    match descriptor.kind {
        ParamKind::Continuous { default, .. } => default,
        ParamKind::Stepped { .. } => unreachable!("{} is declared Continuous", descriptor.key),
    }
}

/// Reads a `Stepped` descriptor's default as "index 1 (On) selected", the same
/// stepped-value-is-the-index convention `ParamChange`'s own doc comment states. Panicking arm is
/// defensive-only, as in [`continuous_default`].
fn stepped_default_on(descriptor: namir_params::ParamDescriptor) -> bool {
    match descriptor.kind {
        ParamKind::Stepped { default_index, .. } => default_index.0 == 1,
        ParamKind::Continuous { .. } => unreachable!("{} is declared Stepped", descriptor.key),
    }
}

/// Builds [`EqStage`]. Holds no configuration of its own — every one of Eq's twelve parameters
/// seeds its initial value straight from its `namir-params` descriptor (see `prepare`'s body), so
/// there is nothing for a caller to pass in here.
pub struct EqPrep;

impl StagePrep for EqPrep {
    type Prepared = EqStage;

    /// Builds one independent five-band `Biquad` cascade per output channel, each already
    /// designed (jumped, not ramped — there is no prior audio at stage construction) to the
    /// descriptor defaults' target coefficients.
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        let sample_rate = ctx.sample_rate();
        let channel_count = ctx.channel_config().output_channels() as usize;

        let mut stage = EqStage {
            enabled: stepped_default_on(ENABLED),
            low_shelf_freq_hz: continuous_default(LOW_SHELF_FREQ_HZ),
            low_shelf_gain_db: continuous_default(LOW_SHELF_GAIN_DB),
            mid_freq_hz: continuous_default(MID_FREQ_HZ),
            mid_gain_db: continuous_default(MID_GAIN_DB),
            mid_q: continuous_default(MID_Q),
            high_shelf_freq_hz: continuous_default(HIGH_SHELF_FREQ_HZ),
            high_shelf_gain_db: continuous_default(HIGH_SHELF_GAIN_DB),
            high_pass_enabled: stepped_default_on(HIGH_PASS_ENABLED),
            high_pass_freq_hz: continuous_default(HIGH_PASS_FREQ_HZ),
            low_pass_enabled: stepped_default_on(LOW_PASS_ENABLED),
            low_pass_freq_hz: continuous_default(LOW_PASS_FREQ_HZ),
            sample_rate,
            max_block_size: ctx.max_block_size(),
            // `Biquad` is deliberately not `Clone`/`Copy` (owns interpolation state that must
            // never be accidentally duplicated), so this builds each channel's cascade from a
            // fresh `Biquad::new()` per slot rather than `vec![[Biquad::new(); 5]; channel_count]`
            // (which would need `[Biquad; 5]: Clone` and does not compile).
            biquads: (0..channel_count)
                .map(|_| {
                    [
                        Biquad::new(),
                        Biquad::new(),
                        Biquad::new(),
                        Biquad::new(),
                        Biquad::new(),
                    ]
                })
                .collect(),
        };

        // Jump (ramp_samples = 0) every band straight to its descriptor-default target: no prior
        // audio exists yet at stage construction, so there is nothing to click against.
        for band in [
            BAND_HIGH_PASS,
            BAND_LOW_SHELF,
            BAND_MID,
            BAND_HIGH_SHELF,
            BAND_LOW_PASS,
        ] {
            let target = stage.band_target(band);
            for channel_bands in &mut stage.biquads {
                channel_bands[band].set_coeffs(target, 0);
            }
        }

        Ok(stage)
    }
}

/// RT-safe five-band parametric EQ, one independent `Biquad` cascade per channel (see this
/// module's doc comment for why Eq is per-channel rather than mono-core), behind FR-CHAIN-020's
/// click-free per-stage bypass expressed as a coefficient-interpolation target rather than a
/// dry/wet crossfade.
pub struct EqStage {
    /// FR-CHAIN-020's per-stage bypass for this stage. When `false`, every band's target
    /// coefficients are [`BiquadCoeffs::identity`] regardless of the other fields below; the
    /// underlying parameter values are still tracked so re-enabling restores them exactly.
    enabled: bool,
    /// FR-EQ-010 low band corner, Hz.
    low_shelf_freq_hz: f32,
    /// FR-EQ-010 low band gain, dB.
    low_shelf_gain_db: f32,
    /// FR-EQ-010 mid band center, Hz.
    mid_freq_hz: f32,
    /// FR-EQ-010 mid band gain, dB.
    mid_gain_db: f32,
    /// FR-EQ-010 mid band Q.
    mid_q: f32,
    /// FR-EQ-010 high band corner, Hz.
    high_shelf_freq_hz: f32,
    /// FR-EQ-010 high band gain, dB.
    high_shelf_gain_db: f32,
    /// FR-EQ-010's defeatable high-pass "on" state — independent of `enabled` (both must be true
    /// for the high-pass band's target to be anything other than identity; see `band_target`).
    high_pass_enabled: bool,
    /// FR-EQ-010 high-pass corner, Hz.
    high_pass_freq_hz: f32,
    /// FR-EQ-010's defeatable low-pass "on" state; see `high_pass_enabled`.
    low_pass_enabled: bool,
    /// FR-EQ-010 low-pass corner, Hz.
    low_pass_freq_hz: f32,
    /// Needed by `apply` to redesign a band's coefficients from its (possibly just-changed)
    /// parameter fields — `BiquadCoeffs::design` is a pure function of this plus the band's own
    /// freq/gain/Q, computed in `f64` per D-9.10.
    sample_rate: SampleRate,
    /// D-9.9's ramp length for any coefficient retarget `apply` triggers. `apply` has no `io` to
    /// read an actual block's frame count from (it runs off `Chain::apply`, not `process`), so
    /// this — the block-size ceiling every buffer was sized to in `prepare` — is used as a safe
    /// upper bound: at most a slightly slower-than-one-block ramp for a sub-maximum host block,
    /// never a click.
    max_block_size: usize,
    /// Per-channel, per-band cascade: `biquads[channel][band]`, band order fixed by the
    /// `BAND_*` consts. One independent `Biquad` (own `s1`/`s2` state) per channel; all channels'
    /// same-index band shares its coefficient *target*, set together by `retarget`.
    biquads: Vec<[Biquad; BAND_COUNT]>,
}

impl EqStage {
    /// The coefficient target `band` should be driven towards right now, given the current
    /// parameter fields: [`BiquadCoeffs::identity`] whenever this stage (or, for the HP/LP bands
    /// only, that band's own defeat toggle) is off, the designed filter otherwise. Pure — does
    /// not touch `biquads`; see `retarget`, which applies this to every channel.
    fn band_target(&self, band: usize) -> BiquadCoeffs {
        match band {
            BAND_HIGH_PASS => {
                if self.enabled && self.high_pass_enabled {
                    BiquadCoeffs::design(
                        FilterKind::HighPass,
                        self.high_pass_freq_hz as f64,
                        HIGH_PASS_LOW_PASS_Q,
                        0.0,
                        self.sample_rate,
                    )
                } else {
                    BiquadCoeffs::identity()
                }
            }
            BAND_LOW_SHELF => {
                if self.enabled {
                    BiquadCoeffs::design(
                        FilterKind::LowShelf,
                        self.low_shelf_freq_hz as f64,
                        SHELF_Q_UNUSED,
                        self.low_shelf_gain_db as f64,
                        self.sample_rate,
                    )
                } else {
                    BiquadCoeffs::identity()
                }
            }
            BAND_MID => {
                if self.enabled {
                    BiquadCoeffs::design(
                        FilterKind::Peaking,
                        self.mid_freq_hz as f64,
                        self.mid_q as f64,
                        self.mid_gain_db as f64,
                        self.sample_rate,
                    )
                } else {
                    BiquadCoeffs::identity()
                }
            }
            BAND_HIGH_SHELF => {
                if self.enabled {
                    BiquadCoeffs::design(
                        FilterKind::HighShelf,
                        self.high_shelf_freq_hz as f64,
                        SHELF_Q_UNUSED,
                        self.high_shelf_gain_db as f64,
                        self.sample_rate,
                    )
                } else {
                    BiquadCoeffs::identity()
                }
            }
            BAND_LOW_PASS => {
                if self.enabled && self.low_pass_enabled {
                    BiquadCoeffs::design(
                        FilterKind::LowPass,
                        self.low_pass_freq_hz as f64,
                        HIGH_PASS_LOW_PASS_Q,
                        0.0,
                        self.sample_rate,
                    )
                } else {
                    BiquadCoeffs::identity()
                }
            }
            _ => unreachable!("band index out of range: {band}"),
        }
    }

    /// Recomputes `band`'s target from the current fields and starts every channel's
    /// corresponding `Biquad` ramping towards it over [`EqStage::max_block_size`] samples
    /// (D-9.9). Called from `apply` for the one or five bands a given parameter change affects.
    fn retarget(&mut self, band: usize) {
        let target = self.band_target(band);
        let ramp_samples = self.max_block_size as u32;
        for channel_bands in &mut self.biquads {
            channel_bands[band].set_coeffs(target, ramp_samples);
        }
    }

    /// Retargets every band — used by the `ENABLED` toggle, since bypassing/engaging this stage
    /// affects all five bands at once.
    fn retarget_all(&mut self) {
        for band in [
            BAND_HIGH_PASS,
            BAND_LOW_SHELF,
            BAND_MID,
            BAND_HIGH_SHELF,
            BAND_LOW_PASS,
        ] {
            self.retarget(band);
        }
    }
}

impl Stage for EqStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        // Every channel independently: no scratch buffer needed (unlike Gate/Nam's mono-core
        // channel-0-then-duplicate pattern) since this stage never reads one channel while
        // writing another -- each channel's own five biquads run entirely in place on that same
        // channel's slice.
        for (ch, bands) in self.biquads.iter_mut().enumerate() {
            let buf = io.channel(ch);
            bands[BAND_HIGH_PASS].process(buf);
            bands[BAND_LOW_SHELF].process(buf);
            bands[BAND_MID].process(buf);
            bands[BAND_HIGH_SHELF].process(buf);
            bands[BAND_LOW_PASS].process(buf);
        }
    }

    fn reset(&mut self) {
        // Per-channel filter memory only -- a reset is a transport stop/reposition, not a
        // parameter or bypass-state change (matches `trim.rs`/`gate.rs`'s identical treatment).
        for channel_bands in &mut self.biquads {
            for band in channel_bands.iter_mut() {
                band.reset();
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        0
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == ENABLED_ID {
            // Stepped param value is the index as f32 (`ParamChange`'s own doc comment); index 1
            // is "On" per `ENABLED`'s descriptor.
            self.enabled = change.value >= 0.5;
            self.retarget_all();
        } else if change.id == LOW_SHELF_FREQ_HZ_ID {
            self.low_shelf_freq_hz = change.value;
            self.retarget(BAND_LOW_SHELF);
        } else if change.id == LOW_SHELF_GAIN_DB_ID {
            self.low_shelf_gain_db = change.value;
            self.retarget(BAND_LOW_SHELF);
        } else if change.id == MID_FREQ_HZ_ID {
            self.mid_freq_hz = change.value;
            self.retarget(BAND_MID);
        } else if change.id == MID_GAIN_DB_ID {
            self.mid_gain_db = change.value;
            self.retarget(BAND_MID);
        } else if change.id == MID_Q_ID {
            self.mid_q = change.value;
            self.retarget(BAND_MID);
        } else if change.id == HIGH_SHELF_FREQ_HZ_ID {
            self.high_shelf_freq_hz = change.value;
            self.retarget(BAND_HIGH_SHELF);
        } else if change.id == HIGH_SHELF_GAIN_DB_ID {
            self.high_shelf_gain_db = change.value;
            self.retarget(BAND_HIGH_SHELF);
        } else if change.id == HIGH_PASS_ENABLED_ID {
            self.high_pass_enabled = change.value >= 0.5;
            self.retarget(BAND_HIGH_PASS);
        } else if change.id == HIGH_PASS_FREQ_HZ_ID {
            self.high_pass_freq_hz = change.value;
            self.retarget(BAND_HIGH_PASS);
        } else if change.id == LOW_PASS_ENABLED_ID {
            self.low_pass_enabled = change.value >= 0.5;
            self.retarget(BAND_LOW_PASS);
        } else if change.id == LOW_PASS_FREQ_HZ_ID {
            self.low_pass_freq_hz = change.value;
            self.retarget(BAND_LOW_PASS);
        }
    }

    /// No FRS Must telemetry is called out for Eq in M2 (this module's own task scope); still
    /// required by the trait.
    fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::{ChannelConfig, SampleRate as CoreSampleRate, db_to_linear, linear_to_db};

    fn ctx(channel_config: ChannelConfig) -> PrepareContext {
        PrepareContext::new(CoreSampleRate::new(48_000).unwrap(), 64, channel_config).unwrap()
    }

    fn stage(channel_config: ChannelConfig) -> EqStage {
        EqPrep.prepare(&ctx(channel_config)).unwrap()
    }

    /// Processes `total` samples of a constant `value` through a mono stage in
    /// `PrepareContext`-respecting 64-sample chunks, returning the last output sample. A constant
    /// (0 Hz) input's steady-state output ratio is exactly a cascade's DC gain.
    fn process_constant_in_chunks(stage: &mut EqStage, total: usize, value: f32) -> f32 {
        let mut buf = vec![value; total];
        let mut offset = 0usize;
        while offset < buf.len() {
            let end = (offset + 64).min(buf.len());
            let n = end - offset;
            let mut channels: [&mut [f32]; 1] = [&mut buf[offset..end]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            offset = end;
        }
        buf[buf.len() - 1]
    }

    /// Processes `total` samples of `value * (-1)^n` through a mono stage in 64-sample chunks,
    /// returning `(last output sample, last input sample)`. This alternating sequence is exactly
    /// the sampled-domain signal at Nyquist (period 2 samples), so its steady-state output/input
    /// ratio is a cascade's Nyquist gain -- the time-domain counterpart of evaluating `H(z=-1)`,
    /// mirroring `biquad.rs`'s own `nyquist_gain` in spirit without needing that crate's
    /// private test-only helper.
    fn process_alternating_in_chunks(stage: &mut EqStage, total: usize, value: f32) -> (f32, f32) {
        let mut buf = vec![0.0f32; total];
        for (n, s) in buf.iter_mut().enumerate() {
            *s = if n % 2 == 0 { value } else { -value };
        }
        let last_input = buf[total - 1];
        let mut offset = 0usize;
        while offset < buf.len() {
            let end = (offset + 64).min(buf.len());
            let n = end - offset;
            let mut channels: [&mut [f32]; 1] = [&mut buf[offset..end]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            offset = end;
        }
        (buf[total - 1], last_input)
    }

    // --- DC/Nyquist gain algebra, driven through apply() (mirrors biquad.rs's own DC/Nyquist
    // test style, minus that crate's private test-only dc_gain/nyquist_gain helpers).

    // trace-partial: FR-EQ-010
    // uncovered: FR-EQ-010 — the method's "magnitude response against the analytic target within
    // uncovered: 0.1 dB" is executed at no band's own frequency: every EQ-stage test measures
    // uncovered: gain at DC and at Nyquist only, against hand-written constants at 0.3 dB, so the
    // uncovered: low-shelf corner (40-500 Hz), the mid peak (200 Hz-5 kHz), the high-shelf corner
    // uncovered: (1-12 kHz) and the 0.2-5.0 Q range's effect on magnitude are unmeasured;
    // uncovered: closes M9b
    #[test]
    fn low_shelf_gain_reflected_at_dc_and_flat_at_nyquist() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: LOW_SHELF_GAIN_DB_ID,
            value: 9.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_in_chunks(&mut stage, 48_000, dc);
        let dc_db = linear_to_db(dc_tail / dc);
        assert!((dc_db - 9.0).abs() < 0.3, "dc_db={dc_db}, expected ~9.0");

        let (nyq_out, nyq_in) = process_alternating_in_chunks(&mut stage, 4_800, dc);
        let nyq_db = linear_to_db((nyq_out / nyq_in).abs());
        assert!(nyq_db.abs() < 0.3, "nyquist_db={nyq_db}, expected ~0");
    }

    #[test]
    fn high_shelf_gain_reflected_at_nyquist_and_flat_at_dc() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: HIGH_SHELF_GAIN_DB_ID,
            value: -12.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_in_chunks(&mut stage, 48_000, dc);
        let dc_db = linear_to_db(dc_tail / dc);
        assert!(dc_db.abs() < 0.3, "dc_db={dc_db}, expected ~0");

        let (nyq_out, nyq_in) = process_alternating_in_chunks(&mut stage, 4_800, dc);
        let nyq_db = linear_to_db((nyq_out / nyq_in).abs());
        assert!(
            (nyq_db - (-12.0)).abs() < 0.3,
            "nyquist_db={nyq_db}, expected ~-12.0"
        );
    }

    #[test]
    fn mid_peaking_is_flat_at_dc_and_nyquist() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: MID_GAIN_DB_ID,
            value: 10.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_in_chunks(&mut stage, 48_000, dc);
        let dc_db = linear_to_db(dc_tail / dc);
        assert!(dc_db.abs() < 0.3, "dc_db={dc_db}, expected ~0");

        let (nyq_out, nyq_in) = process_alternating_in_chunks(&mut stage, 4_800, dc);
        let nyq_db = linear_to_db((nyq_out / nyq_in).abs());
        assert!(nyq_db.abs() < 0.3, "nyquist_db={nyq_db}, expected ~0");
    }

    #[test]
    fn high_pass_enabled_blocks_dc_and_passes_near_nyquist() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: HIGH_PASS_ENABLED_ID,
            value: 1.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_in_chunks(&mut stage, 48_000, dc);
        assert!(
            (dc_tail / dc).abs() < 1e-2,
            "expected DC heavily attenuated once enabled, got ratio {}",
            dc_tail / dc
        );

        let (nyq_out, nyq_in) = process_alternating_in_chunks(&mut stage, 4_800, dc);
        let nyq_db = linear_to_db((nyq_out / nyq_in).abs());
        assert!(
            nyq_db.abs() < 0.3,
            "nyquist_db={nyq_db}, expected ~0 (passed)"
        );
    }

    #[test]
    fn low_pass_enabled_passes_dc_and_blocks_near_nyquist() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: LOW_PASS_ENABLED_ID,
            value: 1.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_in_chunks(&mut stage, 48_000, dc);
        let dc_db = linear_to_db(dc_tail / dc);
        assert!(dc_db.abs() < 0.3, "dc_db={dc_db}, expected ~0 (passed)");

        let (nyq_out, nyq_in) = process_alternating_in_chunks(&mut stage, 4_800, dc);
        assert!(
            (nyq_out / nyq_in).abs() < 1e-2,
            "expected Nyquist heavily attenuated once enabled, got ratio {}",
            nyq_out / nyq_in
        );
    }

    #[test]
    fn disabled_bands_default_to_identity_passthrough() {
        // Descriptor defaults: ENABLED on, HP/LP off, all gains 0 dB -- an all-defaults stage
        // should pass a DC signal through essentially unattenuated.
        let mut stage = stage(ChannelConfig::Mono);
        let dc = 0.3f32;
        let tail = process_constant_in_chunks(&mut stage, 4_800, dc);
        assert!(
            (tail - dc).abs() < 1e-3,
            "expected near-unity passthrough at defaults, got {tail} vs {dc}"
        );
    }

    // --- Multi-channel independence: per-channel filter state must not leak across channels.

    #[test]
    fn channels_are_filtered_independently_no_state_leak() {
        let mut stage = stage(ChannelConfig::Stereo);
        // A resonant mid boost so an impulse leaves a measurable ringing tail in the filter's
        // internal state -- the state a leak would smuggle across channels.
        stage.apply(ParamChange {
            id: MID_GAIN_DB_ID,
            value: 12.0,
        });
        stage.apply(ParamChange {
            id: MID_Q_ID,
            value: 5.0,
        });

        let mut left_rang = false;
        for block in 0..20 {
            let mut left = [0.0f32; 64];
            if block == 0 {
                left[0] = 1.0; // impulse on channel 0 only, once.
            }
            let mut right = [0.0f32; 64]; // channel 1 stays silent for the whole test.
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, 64);
            audio_section(|| stage.process(&mut io));

            if io.channel(0).iter().any(|&s| s.abs() > 1e-6) {
                left_rang = true;
            }
            for &s in io.channel(1).iter() {
                assert!(
                    s.abs() < 1e-6,
                    "channel 1 (silent input) leaked nonzero output from channel 0's state: {s}"
                );
            }
        }
        assert!(
            left_rang,
            "expected channel 0's impulse to produce a nonzero filtered response"
        );
    }

    // --- ENABLED-toggle click-freedom (FR-CHAIN-020/FR-EQ-030).

    // trace-partial: FR-CHAIN-020
    // uncovered: FR-CHAIN-020 — of the four stages the "U per stage" method names, the NAM and
    // uncovered: IR bypass toggles are never exercised mid-signal (nam.rs:1080 and ir.rs:1151
    // uncovered: both apply ENABLED=0 before any processing, then assert steady-state
    // uncovered: passthrough), and no test toggles one stage's bypass inside an assembled chain
    // uncovered: to show the others undisturbed; closes M9b
    #[test]
    fn enabled_toggle_mid_signal_has_no_large_single_sample_jump() {
        let mut stage = stage(ChannelConfig::Mono);
        // A large low-shelf boost so a bypass toggle actually changes the DC gain substantially.
        stage.apply(ParamChange {
            id: LOW_SHELF_GAIN_DB_ID,
            value: 15.0,
        });
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });

        let value = 0.3f32;
        let settled_disabled = process_constant_in_chunks(&mut stage, 48_000, value);

        // trace-partial: FR-EQ-030
        // uncovered: FR-EQ-030 — "changing any EQ parameter" spans one of EqStage's twelve: only
        // uncovered: ENABLED is toggled, and no test changes a band gain, a band frequency, mid
        // uncovered: Q, or either high-pass/low-pass defeat or corner frequency and measures
        // uncovered: click-freedom; closes M9b
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 1.0,
        });

        let total = 4_800usize;
        let mut out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let n = 64usize.min(total - offset);
            let mut buf = vec![value; n];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            out.extend_from_slice(io.channel(0));
            offset += n;
        }

        // The magnitude an instantaneous (unramped) switch would jump by: fully-disabled (unity)
        // vs fully-enabled (db_to_linear(15)) steady-state DC gain, applied to `value`.
        let instant_jump = value * (db_to_linear(15.0) - 1.0);

        let mut prev = settled_disabled;
        let mut max_delta = 0.0f32;
        for &s in &out {
            max_delta = max_delta.max((s - prev).abs());
            prev = s;
        }
        assert!(
            max_delta < instant_jump.abs() * 0.5,
            "max_delta={max_delta} not clearly smaller than an instant full jump of {instant_jump}"
        );
        assert!(max_delta > 0.0, "toggle produced no change at all");
    }

    // --- RT safety.

    #[test]
    fn stereo_process_does_not_allocate() {
        let mut stage = stage(ChannelConfig::Stereo);
        stage.apply(ParamChange {
            id: HIGH_PASS_ENABLED_ID,
            value: 1.0,
        });
        stage.apply(ParamChange {
            id: LOW_PASS_ENABLED_ID,
            value: 1.0,
        });
        let mut left = [0.1f32; 64];
        let mut right = [0.2f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }
}
