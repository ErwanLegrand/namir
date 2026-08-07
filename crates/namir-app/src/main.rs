//! The standalone binary's entry point — see `lib.rs` for this crate's full module map and
//! `app::run`'s own doc comment for what actually happens. Kept to a single call so there is
//! exactly one place that decides "this is the real program", as opposed to the library target
//! (`namir_app`) a future integration test or `namir-clap`-adjacent tool could also link against.

fn main() {
    namir_app::app::run();
}
