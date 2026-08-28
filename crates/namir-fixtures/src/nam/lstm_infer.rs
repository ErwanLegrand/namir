//! A minimal LSTM forward pass used *only* to validate generated fixtures (the RMS/degeneracy
//! check D-19.1 requires of the generator itself) and as `namir-nam`'s cross-implementation
//! numeric-parity oracle for its own LSTM implementation — the LSTM counterpart of `infer.rs`.
//! Deliberately not shared with `namir-engine`, not written for the RT audio path (allocates
//! freely, no scratch reuse), and only handles the subset this crate's [`super::generate_lstm`]
//! ever emits (`input_size == in_channels == out_channels == 1`, per `namir-nam`'s own scope
//! restriction — see that crate's `lstm.rs`).
//!
//! # Provenance: derived from `NeuralAmpModelerCore`, not from `namir-nam`
//!
//! **This module is derived from `NeuralAmpModelerCore` directly** — `NAM/lstm.h` and
//! `NAM/lstm.cpp` at the pinned commit `3cde95c354d5ba6da01316cad90b05cfc4855053`, the same
//! commit `crates/namir-nam/tests/golden_reference.rs`'s renders are built from — in the same
//! spirit `a2_infer.rs` states for A2 (R-9, `docs/02-architecture.md` §22). Until M14 it was not:
//! its doc comment said its layout and operation order "follow the same facts `namir-nam`'s
//! `lstm.rs` module doc comment records", i.e. it was written from *that crate's reading* of the
//! format rather than from the format's own source. A parity test between two ports of one
//! reading cannot see a misreading they share, which is what this re-derivation fixes.
//!
//! Every fact this module relies on, each cited to the upstream line that establishes it (line
//! numbers are at the pinned commit):
//!
//! - **Per-cell weight consumption order** (`LSTMCell::LSTMCell`, `lstm.cpp:9-29`): `W` first,
//!   row-major, `(4*hidden_size)` rows by `(cell_input_size + hidden_size)` columns
//!   (`lstm.cpp:12`, `:19-21` — "Assign in row-major because that's how PyTorch goes"); then `b`,
//!   `4*hidden_size` floats (`:22-23`); then a **learned** initial hidden state `h0`,
//!   `hidden_size` floats written straight into `xh`'s tail (`:24-26`) — not a zero init; then a
//!   **learned** initial cell state `c0`, `hidden_size` floats (`:27-28`).
//! - **The state vector is `xh = [x ; h]`** with the hidden state living in its tail, which is
//!   also how a cell's hidden state is read back out (`lstm.h:32-35`,
//!   `_xh.tail(_get_hidden_size())`). `_get_input_size()` is derived as `xh.size() - hidden_size`
//!   (`lstm.h:60`).
//! - **Gate order within the `4*hidden_size` pre-activation vector** (`LSTMCell::process_`,
//!   `lstm.cpp:42-45`): `i` at offset `0`, `f` at `hidden_size`, `g` at `2*hidden_size`, `o` at
//!   `3*hidden_size`.
//! - **The update itself** (`lstm.cpp:61-66`, the `using_fast_tanh == false` branch — the exact
//!   one, which is what this crate ports, matching `namir-nam`'s own choice and FR-NAM-030's
//!   accuracy floor): `ifgo = W @ xh + b` (`:39-40`), then
//!   `c[k] = sigmoid(f[k]) * c[k] + sigmoid(i[k]) * tanh(g[k])` for every `k` **first**
//!   (`:61-63`), and only then `h[k] = sigmoid(o[k]) * tanh(c[k])` reading the just-updated `c`
//!   (`:65-66`). `sigmoid` is the exact `1 / (1 + expf(-x))` (`activations.h:64-67`), not the
//!   `fast_sigmoid` approximation.
//! - **Layer chaining** (`LSTM::_process_sample`, `lstm.cpp:153-155`): layer 0 takes the model's
//!   raw input for the current sample; layer `i > 0` takes layer `i-1`'s hidden state from the
//!   *same* sample, with no delay. Hence layer `i > 0`'s cell is constructed with
//!   `cell_input_size == hidden_size`, not `input_size` (`lstm.cpp:79-80`).
//! - **Head** (`LSTM::LSTM`, `lstm.cpp:84-98`, applied at `:160-167`): after **every** layer's
//!   weights, `head_weight` (`out_channels x hidden_size`, row-major) then `head_bias`
//!   (`out_channels`), and `output = head_weight @ h_last + head_bias`.
//! - **No trailing scalar** after `head_bias`: upstream asserts the weight vector is exactly
//!   exhausted at that point (`assert(it == weights.end())`, `lstm.cpp:100`) — unlike WaveNet's
//!   trailing `head_scale`. This module asserts the same thing for the same reason.
//!
//! Not ported, and not needed by anything here: upstream's zero-layer passthrough
//! (`lstm.cpp:141-151` — this crate's generator always emits at least one layer), its
//! `fast_tanh`/`fast_sigmoid` branch (`:48-58`), and `GetPrewarmSamples` (`:127-134`), which is a
//! caller-side warm-up recommendation rather than part of the model's definition.
//!
//! **What this provenance does and does not buy.** The facts above are now checked against the
//! source that defines them rather than against another Rust port of it, and `tests` below pins
//! each one against hand-computed arithmetic so a later edit cannot quietly drift back. What it
//! cannot claim is blind independence: this rewrite replaced an existing port, so its author had
//! seen `namir-nam`'s. The remaining external check on the shared-misreading risk is
//! `namir-nam/tests/golden_reference.rs`'s `lstm_tiny` render, which is a real
//! `NeuralAmpModelerCore` output — and it exercises a **one-layer** model, so the multi-layer
//! facts above (`cell_input_size == hidden_size` for layer `i > 0`, same-sample chaining) are
//! covered by this module's analytic tests and by no external render.
//!
//! # Float summation order is deliberately preserved
//!
//! Both accumulations below (the `W @ xh + b` row dot product, and the head) start from the bias
//! and add products into it, which is not upstream's association (`_ifgo = W * xh` then
//! `+= b`; `_output = head_weight * h` then `+= head_bias`). The difference is last-ulp only, but
//! [`super::generate_lstm`]'s calibration pass measures *this* function's output RMS to pick its
//! head-rescale factor, so a last-ulp change here changes the weights of every generated LSTM
//! fixture — including the checked-in `crates/namir-nam/tests/golden/lstm_tiny.nam`, whose
//! reference render was produced from those exact bytes. Preserving the order keeps every
//! generated fixture byte-identical across this rewrite.

use super::{LstmConfig, LstmModel};

struct WeightReader<'a> {
    weights: &'a [f32],
    pos: usize,
}

impl<'a> WeightReader<'a> {
    fn take(&mut self, n: usize) -> &'a [f32] {
        let slice = &self.weights[self.pos..self.pos + n];
        self.pos += n;
        slice
    }
}

/// `activations::sigmoid`, `activations.h:64-67`.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// One LSTM cell: upstream's `LSTMCell` (`lstm.h:17-61`), weights and evolving state together,
/// with the hidden state living in `xh`'s tail exactly as it does there.
struct LstmCell<'a> {
    /// This cell's own input width — `input_size` for layer 0, `hidden_size` for every later
    /// layer (`lstm.cpp:79-80`).
    input_size: usize,
    hidden_size: usize,
    /// `(4*hidden_size)` rows by `(input_size + hidden_size)` columns, row-major.
    w: &'a [f32],
    b: &'a [f32],
    /// `[x ; h]`: the current input in `[0, input_size)`, the hidden state in the tail. Seeded
    /// from the learned `h0`.
    xh: Vec<f32>,
    /// Cell state, seeded from the learned `c0`.
    c: Vec<f32>,
    /// Scratch for the four gates' pre-activations, `4*hidden_size` long.
    ifgo: Vec<f32>,
}

impl<'a> LstmCell<'a> {
    /// `LSTMCell`'s constructor, `lstm.cpp:9-29`: `W`, `b`, `h0` (into `xh`'s tail), `c0`.
    fn read(input_size: usize, hidden_size: usize, r: &mut WeightReader<'a>) -> Self {
        let w = r.take(4 * hidden_size * (input_size + hidden_size));
        let b = r.take(4 * hidden_size);
        let h0 = r.take(hidden_size);
        let c0 = r.take(hidden_size);

        let mut xh = vec![0f32; input_size + hidden_size];
        xh[input_size..].copy_from_slice(h0);

        Self {
            input_size,
            hidden_size,
            w,
            b,
            xh,
            c: c0.to_vec(),
            ifgo: vec![0f32; 4 * hidden_size],
        }
    }

    /// `LSTMCell::process_`, `lstm.cpp:31-68` (the exact-math branch).
    fn process(&mut self, x: &[f32]) {
        let (input_size, hidden_size) = (self.input_size, self.hidden_size);
        let cols = input_size + hidden_size;
        self.xh[..input_size].copy_from_slice(x);

        // ifgo = W @ xh + b (`lstm.cpp:39-40`; see this module's note on summation order).
        for (row, slot) in self.ifgo.iter_mut().enumerate() {
            let mut acc = self.b[row];
            for (wv, xv) in self.w[row * cols..(row + 1) * cols].iter().zip(&self.xh) {
                acc += wv * xv;
            }
            *slot = acc;
        }

        // Gate offsets, `lstm.cpp:42-45`.
        let (i_off, f_off, g_off, o_off) = (0, hidden_size, 2 * hidden_size, 3 * hidden_size);
        // Every c[k] first (`lstm.cpp:61-63`) ...
        for k in 0..hidden_size {
            self.c[k] = sigmoid(self.ifgo[f_off + k]) * self.c[k]
                + sigmoid(self.ifgo[i_off + k]) * self.ifgo[g_off + k].tanh();
        }
        // ... then every h[k], from the just-updated c (`lstm.cpp:65-66`).
        for k in 0..hidden_size {
            self.xh[input_size + k] = sigmoid(self.ifgo[o_off + k]) * self.c[k].tanh();
        }
    }

    /// `LSTMCell::get_hidden_state`, `lstm.h:32-35`: the tail of `xh`.
    fn hidden_state(&self) -> &[f32] {
        &self.xh[self.input_size..]
    }
}

/// Runs `model` over `input` (mono, `input_size == 1`) and returns the mono output (`out_channels
/// == 1`) — upstream's `LSTM` constructor (`lstm.cpp:70-101`) followed by `LSTM::process` /
/// `LSTM::_process_sample` (`:103-125`, `:136-168`). Panics on malformed weight counts —
/// acceptable here because this function only ever runs against this crate's own generator
/// output, never external input.
pub(super) fn run(model: &LstmModel, input: &[f32]) -> Vec<f32> {
    let n = input.len();
    let cfg: &LstmConfig = &model.config;
    let mut r = WeightReader {
        weights: &model.weights,
        pos: 0,
    };

    // `lstm.cpp:79-80`: layer 0's cell input is the model input, every later layer's is a hidden
    // state.
    let mut layers: Vec<LstmCell> = (0..cfg.num_layers)
        .map(|i| {
            let cell_input_size = if i == 0 {
                cfg.input_size
            } else {
                cfg.hidden_size
            };
            LstmCell::read(cell_input_size, cfg.hidden_size, &mut r)
        })
        .collect();
    assert!(
        !layers.is_empty(),
        "zero-layer LSTM: upstream's passthrough branch is not ported (see the module doc \
         comment); this crate's generator never emits one"
    );

    let head_weight = r.take(cfg.out_channels * cfg.hidden_size);
    let head_bias = r.take(cfg.out_channels);
    // `lstm.cpp:100`'s `assert(it == weights.end())`: no trailing scalar, unlike WaveNet.
    assert_eq!(r.pos, model.weights.len(), "unexpected weight count");

    let mut out = vec![0f32; cfg.out_channels * n];
    let mut hop = vec![0f32; cfg.hidden_size];
    for t in 0..n {
        // `_process_sample`, `lstm.cpp:153-155`: all layers advance within one sample, each
        // reading the previous layer's hidden state as it stands after that same sample.
        let x: Vec<f32> = (0..cfg.input_size).map(|ch| input[ch * n + t]).collect();
        layers[0].process(&x);
        for i in 1..layers.len() {
            hop.copy_from_slice(layers[i - 1].hidden_state());
            layers[i].process(&hop);
        }

        // `lstm.cpp:160-167`: head applied to the last layer's hidden state.
        let h = layers[cfg.num_layers - 1].hidden_state();
        for oc in 0..cfg.out_channels {
            let hw = &head_weight[oc * cfg.hidden_size..(oc + 1) * cfg.hidden_size];
            let mut acc = head_bias[oc];
            for k in 0..cfg.hidden_size {
                acc += hw[k] * h[k];
            }
            out[oc * n + t] = acc;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nam::NamMetadata;

    /// The smallest model shape that still exercises the whole contract, wrapped so a test only
    /// has to supply a weight vector.
    fn model(num_layers: usize, hidden_size: usize, weights: Vec<f32>) -> LstmModel {
        LstmModel {
            version: "0.5.5".to_string(),
            architecture: "LSTM".to_string(),
            config: LstmConfig {
                num_layers,
                input_size: 1,
                hidden_size,
                in_channels: 1,
                out_channels: 1,
            },
            weights,
            sample_rate: 48_000,
            metadata: NamMetadata {
                name: "lstm_infer analytic test".to_string(),
                modeled_by: "namir-fixtures".to_string(),
                gear_type: "amp".to_string(),
                tone_type: "clean".to_string(),
                description: "hand-computed contract fixture".to_string(),
            },
        }
    }

    /// One `hidden_size == 1` cell step, written out as scalar arithmetic straight from
    /// `lstm.cpp:39-66` — deliberately a *different formulation* from [`LstmCell::process`]
    /// (no offsets, no loops, gates named), so it agrees with the implementation only if both
    /// read the format the same way. `w` is `[[w_i_x, w_i_h], [w_f..], [w_g..], [w_o..]]`
    /// flattened row-major, exactly as upstream stores it.
    fn hand_step(
        w: [f32; 8],
        b: [f32; 4],
        h_prev: f32,
        c_prev: f32,
        x: f32,
        gate_order: [usize; 4],
    ) -> (f32, f32) {
        // `gate_order[j]` is the row the j-th gate (i, f, g, o) is read from — the correct
        // reading is `[0, 1, 2, 3]`; the other permutations are the misreadings the assertions
        // below require this fixture to be able to tell apart.
        let pre = |row: usize| b[row] + w[2 * row] * x + w[2 * row + 1] * h_prev;
        let (zi, zf, zg, zo) = (
            pre(gate_order[0]),
            pre(gate_order[1]),
            pre(gate_order[2]),
            pre(gate_order[3]),
        );
        let c = sigmoid(zf) * c_prev + sigmoid(zi) * zg.tanh();
        let h = sigmoid(zo) * c.tanh();
        (h, c)
    }

    const W1: [f32; 8] = [0.5, -0.25, 0.75, 0.125, -0.5, 0.625, 0.25, -0.75];
    const B1: [f32; 4] = [0.1, -0.2, 0.3, -0.4];
    const H0: f32 = 0.35;
    const C0: f32 = -0.15;
    const HEAD_W: f32 = 1.5;
    const HEAD_B: f32 = -0.05;

    fn single_layer_weights() -> Vec<f32> {
        let mut v = Vec::new();
        v.extend_from_slice(&W1);
        v.extend_from_slice(&B1);
        v.push(H0);
        v.push(C0);
        v.push(HEAD_W);
        v.push(HEAD_B);
        v
    }

    /// Pins the whole single-layer contract — gate order, the c-then-h update, `h0`/`c0` being
    /// *learned weights* rather than zero init, the head, and the absence of a trailing scalar —
    /// against arithmetic worked out from `lstm.cpp` rather than against another port of it.
    #[test]
    fn a_single_layer_matches_the_upstream_equations_worked_by_hand() {
        let m = model(1, 1, single_layer_weights());
        let input = [0.2f32, -0.6, 0.9];
        let got = run(&m, &input);

        let (mut h, mut c) = (H0, C0);
        let mut want = Vec::new();
        for &x in &input {
            let (h_next, c_next) = hand_step(W1, B1, h, c, x, [0, 1, 2, 3]);
            h = h_next;
            c = c_next;
            want.push(HEAD_B + HEAD_W * h);
        }

        assert_eq!(got.len(), want.len());
        for (t, (&g, &w)) in got.iter().zip(&want).enumerate() {
            assert!(
                (g - w).abs() < 1e-6,
                "sample {t}: got {g}, hand-computed {w}"
            );
        }
    }

    /// The fixture above is only evidence if it can tell the plausible misreadings apart, so this
    /// asserts that it does: each variant below is what the same weights would produce under one
    /// specific wrong reading of the format, and each must differ materially from the right one.
    #[test]
    fn the_hand_computed_fixture_discriminates_the_plausible_misreadings() {
        let x = 0.2f32;
        let (h_correct, _) = hand_step(W1, B1, H0, C0, x, [0, 1, 2, 3]);

        // Gate order f,i,g,o (i and f transposed) -- the classic one, since PyTorch's own
        // documentation lists the gates in that order in places.
        let (h_if_swapped, _) = hand_step(W1, B1, H0, C0, x, [1, 0, 2, 3]);
        // Gate order i,f,o,g (the candidate and output gates transposed).
        let (h_go_swapped, _) = hand_step(W1, B1, H0, C0, x, [0, 1, 3, 2]);
        // `h0`/`c0` read in the opposite order.
        let (h_h0c0_swapped, _) = hand_step(W1, B1, C0, H0, x, [0, 1, 2, 3]);
        // `h0`/`c0` treated as zero init rather than as learned weights.
        let (h_zero_init, _) = hand_step(W1, B1, 0.0, 0.0, x, [0, 1, 2, 3]);

        for (name, other) in [
            ("i/f swapped", h_if_swapped),
            ("g/o swapped", h_go_swapped),
            ("h0/c0 swapped", h_h0c0_swapped),
            ("zero-initialised state", h_zero_init),
        ] {
            assert!(
                (h_correct - other).abs() > 1e-3,
                "the fixture cannot tell the correct reading from `{name}` ({h_correct} vs \
                 {other}) -- pick different weights"
            );
        }
    }

    /// Layer `i > 0` takes the previous layer's hidden state from the **same** sample
    /// (`lstm.cpp:153-155`). A one-sample delay there is invisible in a steady state but not at
    /// the start of the signal, which is what this compares.
    #[test]
    fn two_layers_chain_within_one_sample() {
        // Layer 1 has `cell_input_size == hidden_size == 1` here, so its weight block has the
        // same shape as layer 0's; the width-discrimination case is the test below.
        const W2: [f32; 8] = [-0.4, 0.2, 0.6, -0.3, 0.45, 0.15, -0.2, 0.5];
        const B2: [f32; 4] = [-0.05, 0.15, -0.25, 0.35];
        const H0_2: f32 = -0.2;
        const C0_2: f32 = 0.4;

        let mut weights = Vec::new();
        weights.extend_from_slice(&W1);
        weights.extend_from_slice(&B1);
        weights.push(H0);
        weights.push(C0);
        weights.extend_from_slice(&W2);
        weights.extend_from_slice(&B2);
        weights.push(H0_2);
        weights.push(C0_2);
        weights.push(HEAD_W);
        weights.push(HEAD_B);

        let m = model(2, 1, weights);
        let input = [0.2f32, -0.6, 0.9];
        let got = run(&m, &input);

        let (mut h1, mut c1) = (H0, C0);
        let (mut h2, mut c2) = (H0_2, C0_2);
        let mut want = Vec::new();
        let mut want_delayed = Vec::new();
        let mut h1_prev = H0;
        for &x in &input {
            let (h1n, c1n) = hand_step(W1, B1, h1, c1, x, [0, 1, 2, 3]);
            // Correct: layer 1 consumes `h1n`, this sample's layer-0 output.
            let (h2n, c2n) = hand_step(W2, B2, h2, c2, h1n, [0, 1, 2, 3]);
            // The misreading: layer 1 consumes the *previous* sample's layer-0 output.
            let (h2_delayed, _) = hand_step(W2, B2, h2, c2, h1_prev, [0, 1, 2, 3]);
            h1_prev = h1n;
            h1 = h1n;
            c1 = c1n;
            h2 = h2n;
            c2 = c2n;
            want.push(HEAD_B + HEAD_W * h2n);
            want_delayed.push(HEAD_B + HEAD_W * h2_delayed);
        }

        for (t, (&g, &w)) in got.iter().zip(&want).enumerate() {
            assert!(
                (g - w).abs() < 1e-6,
                "sample {t}: got {g}, hand-computed {w}"
            );
        }
        assert!(
            (want[0] - want_delayed[0]).abs() > 1e-3,
            "this fixture cannot tell same-sample chaining from a one-sample delay"
        );
    }

    /// `lstm.cpp:79-80`: layer `i > 0` is constructed with `cell_input_size == hidden_size`, not
    /// `input_size`. With `hidden_size > input_size` the two readings consume different numbers
    /// of weights, so upstream's "the weight vector is exactly exhausted" assertion
    /// (`lstm.cpp:100`) is what separates them — and nothing external does: the only real
    /// `NeuralAmpModelerCore` render this project holds for LSTM
    /// (`namir-nam/tests/golden/lstm_tiny*`) is a **one-layer** model.
    #[test]
    fn a_later_layer_takes_its_input_width_from_hidden_size() {
        const HIDDEN: usize = 2;
        const INPUT: usize = 1;
        let per_layer =
            |cell_input: usize| 4 * HIDDEN * (cell_input + HIDDEN) + 4 * HIDDEN + 2 * HIDDEN;
        let correct = per_layer(INPUT) + per_layer(HIDDEN) + HIDDEN + 1;
        let misread = per_layer(INPUT) + per_layer(INPUT) + HIDDEN + 1;
        assert_ne!(
            correct, misread,
            "this shape cannot tell the two readings apart -- widen `hidden_size`"
        );

        // Weight values are irrelevant to the count; a simple deterministic ramp keeps the model
        // non-degenerate.
        let weights: Vec<f32> = (0..correct).map(|i| ((i % 7) as f32 - 3.0) * 0.1).collect();
        let m = model(2, HIDDEN, weights);
        let out = run(&m, &[0.2, -0.6, 0.9]);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
