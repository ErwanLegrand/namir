//! Small helpers shared by every architecture module (`wavenet.rs`, `lstm.rs`): a flat-array
//! weight reader, the two NFR-SEC-020 ceiling-check primitives, and (added with issue #49) the
//! two finiteness checks. Factored out here rather than left as `wavenet.rs`-private (which is
//! how they started — WaveNet was implemented first) because `lstm.rs` needs exactly the same
//! things: "read `n` floats off the front of the model's flat weight array, or fail cleanly",
//! "reject a declared dimension before it's used in any arithmetic", and "reject a weight that
//! isn't a finite number before it can reach the audio thread." No helper here knows anything
//! about either architecture's layout, so sharing them is not an abstraction stretch — it would
//! have been duplication otherwise.

use crate::error_codes::{self, NamLoadError};

/// Sequentially consumes floats off the front of a model's flat `weights` array, the same way
/// for every architecture: each `.nam` format's own doc comment (`wavenet.rs`'s, `lstm.rs`'s)
/// specifies the exact order its fields are read in; this type only provides the "read `n`, or
/// fail with `WEIGHT_COUNT_MISMATCH`" primitive both rely on.
pub(crate) struct WeightReader<'a> {
    weights: &'a [f32],
    pub(crate) pos: usize,
}

impl<'a> WeightReader<'a> {
    pub(crate) fn new(weights: &'a [f32]) -> Self {
        Self { weights, pos: 0 }
    }

    /// Copies out `n` floats starting at the reader's current position and advances it. Every
    /// call site passes an `n` built only from dimensions already checked against the caller's
    /// own NFR-SEC-020 ceilings (`wavenet.rs`'s `validate_layer_array_dims`, `lstm.rs`'s
    /// equivalent), so `self.pos + n` cannot overflow `usize` on any 64-bit target — the
    /// bound-inputs-first ordering in each architecture's `from_file` is what makes that true,
    /// not a `checked_add` here.
    pub(crate) fn take(&mut self, n: usize) -> Result<Vec<f32>, NamLoadError> {
        if self.pos + n > self.weights.len() {
            return Err(NamLoadError {
                code: error_codes::WEIGHT_COUNT_MISMATCH,
                detail: format!(
                    "weight array exhausted: need {n} more floats at offset {}, only {} available",
                    self.pos,
                    self.weights.len().saturating_sub(self.pos)
                ),
            });
        }
        let slice = self.weights[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(slice)
    }
}

/// NFR-SEC-020: rejects `value` if it exceeds `max`, before any arithmetic or allocation is
/// derived from it — see each caller's own dimension-ceiling constants for why the specific
/// `max` passed in is generous relative to any plausible real export.
pub(crate) fn check_max(value: usize, max: usize, name: &str) -> Result<(), NamLoadError> {
    if value > max {
        return Err(NamLoadError {
            code: error_codes::DIMENSION_LIMIT_EXCEEDED,
            detail: format!("{name} = {value} exceeds the maximum of {max}"),
        });
    }
    Ok(())
}

/// Rejects any non-finite float (infinity or NaN) in `values`, naming the first offender's index.
/// `name` identifies the array to the user (`"weights"`, an activation parameter, ...).
///
/// Why this is a *load-time* check and not an audio-thread one: `serde_json` deserializes `1e40`
/// — in `f64` range, out of `f32` range — to `f32::INFINITY` without error, so a real file can
/// carry one. A single non-finite weight propagates through inference to a non-finite output
/// block, which FR-CHAIN-080/090's guard downstream then mutes; the user gets permanent silence
/// and a fault counter instead of FR-NAM-040's message naming the reason. The audio thread cannot
/// return an error, and this one is free to detect here — the weights are already in hand, and
/// their length is already bounded by the caller's own `MAX_*_TOTAL_WEIGHTS` ceiling.
pub(crate) fn check_finite(values: &[f32], name: &str) -> Result<(), NamLoadError> {
    if let Some((i, v)) = values.iter().enumerate().find(|(_, v)| !v.is_finite()) {
        return Err(NamLoadError {
            code: error_codes::NON_FINITE_VALUE,
            detail: format!("{name}[{i}] is {v}, which is not a finite number"),
        });
    }
    Ok(())
}

/// The scalar counterpart of [`check_finite`], for a single named float (`config.head_scale`, an
/// activation's `negative_slope`, ...).
pub(crate) fn check_finite_scalar(value: f32, name: &str) -> Result<(), NamLoadError> {
    if !value.is_finite() {
        return Err(NamLoadError {
            code: error_codes::NON_FINITE_VALUE,
            detail: format!("{name} is {value}, which is not a finite number"),
        });
    }
    Ok(())
}

/// Rejects `value == 0`: every dimension this crate reads is used as an array length or a matrix
/// dimension at least once, and zero is never a sensible size for those.
pub(crate) fn check_min1(value: usize, name: &str) -> Result<(), NamLoadError> {
    if value == 0 {
        return Err(NamLoadError {
            code: error_codes::DIMENSION_LIMIT_EXCEEDED,
            detail: format!("{name} must be at least 1, found 0"),
        });
    }
    Ok(())
}
