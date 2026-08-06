//! Small helpers shared by every architecture module (`wavenet.rs`, `lstm.rs`): a flat-array
//! weight reader and the two NFR-SEC-020 ceiling-check primitives. Factored out here rather than
//! left as `wavenet.rs`-private (which is how they started — WaveNet was implemented first)
//! because `lstm.rs` needs exactly the same two things: "read `n` floats off the front of the
//! model's flat weight array, or fail cleanly" and "reject a declared dimension before it's used
//! in any arithmetic." Neither helper knows anything about either architecture's layout, so
//! sharing them is not an abstraction stretch — it would have been duplication otherwise.

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
