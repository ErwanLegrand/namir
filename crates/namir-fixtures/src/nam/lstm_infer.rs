//! A minimal, whole-signal LSTM forward pass used *only* to validate generated fixtures (the
//! RMS/degeneracy check D-19.1 requires of the generator itself) and as `namir-nam`'s
//! cross-implementation numeric-parity oracle for its own LSTM implementation — the LSTM
//! counterpart of `infer.rs`. Deliberately not shared with `namir-engine`, not written for the RT
//! audio path (no blockwise state carried across calls, allocates freely), and only handles the
//! subset this crate's [`super::generate_lstm`] ever emits (`input_size == in_channels ==
//! out_channels == 1`, per `namir-nam`'s own scope restriction — see that crate's `lstm.rs`).
//!
//! Operation order and flat-weight-array layout follow the same facts `namir-nam`'s `lstm.rs`
//! module doc comment records from reading `NeuralAmpModelerCore`'s `NAM/lstm.h`/`NAM/lstm.cpp`
//! directly: per layer, `W` (row-major, `(4*hidden_size) x (cell_input_size+hidden_size)`), `b`,
//! a learned initial hidden state `h0`, a learned initial cell state `c0`; then, after every
//! layer, `head_weight` (`out_channels x hidden_size`, row-major) and `head_bias` — with **no**
//! trailing scalar afterward (unlike WaveNet's `head_scale`).

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

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

struct CellWeights<'a> {
    input_size: usize,
    hidden_size: usize,
    w: &'a [f32],
    b: &'a [f32],
    h0: &'a [f32],
    c0: &'a [f32],
}

/// Runs one LSTM cell over a whole signal at once. `input`: flat `[input_size * n]`. Returns the
/// hidden state at every time step, flat `[hidden_size * n]` — becomes the next layer's `input`.
fn run_cell(w: &CellWeights, input: &[f32], n: usize) -> Vec<f32> {
    let hidden_size = w.hidden_size;
    let input_size = w.input_size;
    let cols = input_size + hidden_size;

    let mut h = w.h0.to_vec();
    let mut c = w.c0.to_vec();
    let mut xh = vec![0f32; cols];
    let mut ifgo = vec![0f32; 4 * hidden_size];
    let mut h_seq = vec![0f32; hidden_size * n];

    for t in 0..n {
        for i in 0..input_size {
            xh[i] = input[i * n + t];
        }
        xh[input_size..].copy_from_slice(&h);

        for (row, slot) in ifgo.iter_mut().enumerate() {
            let mut acc = w.b[row];
            let w_row = &w.w[row * cols..(row + 1) * cols];
            for (wv, xv) in w_row.iter().zip(xh.iter()) {
                acc += wv * xv;
            }
            *slot = acc;
        }

        let (i_off, f_off, g_off, o_off) = (0, hidden_size, 2 * hidden_size, 3 * hidden_size);
        for k in 0..hidden_size {
            let f_gate = sigmoid(ifgo[f_off + k]);
            let i_gate = sigmoid(ifgo[i_off + k]);
            let g_val = ifgo[g_off + k].tanh();
            c[k] = f_gate * c[k] + i_gate * g_val;
        }
        for k in 0..hidden_size {
            let o_gate = sigmoid(ifgo[o_off + k]);
            h[k] = o_gate * c[k].tanh();
        }
        for k in 0..hidden_size {
            h_seq[k * n + t] = h[k];
        }
    }
    h_seq
}

/// Runs `model` over `input` (mono, `input_size == 1`) and returns the mono output (`out_channels
/// == 1`). Panics on malformed weight counts — acceptable here because this function only ever
/// runs against this crate's own generator output, never external input.
pub(super) fn run(model: &LstmModel, input: &[f32]) -> Vec<f32> {
    let n = input.len();
    let cfg: &LstmConfig = &model.config;
    let mut r = WeightReader {
        weights: &model.weights,
        pos: 0,
    };

    let mut cur: Vec<f32> = input.to_vec();
    for i in 0..cfg.num_layers {
        let cell_input_size = if i == 0 {
            cfg.input_size
        } else {
            cfg.hidden_size
        };
        let hidden_size = cfg.hidden_size;
        let rows = 4 * hidden_size;
        let cols = cell_input_size + hidden_size;
        let w = r.take(rows * cols);
        let b = r.take(rows);
        let h0 = r.take(hidden_size);
        let c0 = r.take(hidden_size);
        let cw = CellWeights {
            input_size: cell_input_size,
            hidden_size,
            w,
            b,
            h0,
            c0,
        };
        cur = run_cell(&cw, &cur, n);
    }

    let head_weight = r.take(cfg.out_channels * cfg.hidden_size);
    let head_bias = r.take(cfg.out_channels);
    assert_eq!(r.pos, model.weights.len(), "unexpected weight count");

    let mut out = vec![0f32; cfg.out_channels * n];
    for oc in 0..cfg.out_channels {
        let hw = &head_weight[oc * cfg.hidden_size..(oc + 1) * cfg.hidden_size];
        let b = head_bias[oc];
        for t in 0..n {
            let mut acc = b;
            for k in 0..cfg.hidden_size {
                acc += hw[k] * cur[k * n + t];
            }
            out[oc * n + t] = acc;
        }
    }
    out
}
