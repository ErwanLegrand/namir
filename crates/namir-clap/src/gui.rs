//! CLAP's `gui` extension (FR-CLAP-100): embeds `namir-ui`'s real window via
//! `namir_ui::app::open_parented`, replacing `spikes/s4-clack-clap`'s placeholder egui window
//! with a real [`crate::ui_host::ClapUiHost`] bridging to this instance's live state.
//!
//! # D-5.3's written safety argument for [`NamirMainThread::set_parent`]'s `unsafe` block
//!
//! **What the `unsafe` block does.** `window.borrow_handle_unchecked()` asks `clack_extensions`
//! to reinterpret the host-supplied `clap_window_t` (a tagged union — `win32: HWND`/`cocoa: id`/
//! `x11: XID`, selected by `GuiConfiguration::api_type`) as a
//! [`raw_window_handle::WindowHandle`], the type `namir_ui::app::open_parented`'s `P:
//! raw_window_handle::HasWindowHandle` bound requires. Nothing about that reinterpretation can be
//! checked by the type system or verified from inside this function: `clap_window_t` is a bare C
//! union with no discriminant beyond whatever `GuiConfiguration::api_type` the host separately
//! reported, and the pointer/handle it carries is opaque, foreign, OS-level window state this
//! crate did not create.
//!
//! **What is trusted, and why it is a reasonable trust boundary rather than a gap.** The CLAP
//! specification's own contract for `clap_plugin_gui.set_parent()` (`clap/include/clap/ext/gui.h`)
//! is exactly the contract this function relies on and nothing more:
//!
//! 1. The `clap_window_t` the host passes names a real, live native window of the type declared by
//!    the *matching* `api_type` this plugin itself returned from `is_api_supported`/
//!    `get_preferred_api` — CLAP requires the host to call `set_parent` only with a configuration
//!    the plugin already accepted, so `configuration.api_type == GuiApiType::WIN32` here is not
//!    re-derived from the union, it is the same value this plugin asserted it supports.
//! 2. That window remains valid for the lifetime of the embedded editor — until this plugin's own
//!    `destroy()` runs (which this crate's `PluginGuiImpl::destroy` uses to drop the `WindowHandle`
//!    egui-baseview opened against it) or the host itself is torn down. CLAP's specified plugin
//!    lifecycle guarantees `set_parent` is never called after `destroy`, and a host that violates
//!    either of these two guarantees has already broken the one C ABI contract every CLAP plugin
//!    — not only this one, and not only ones written in Rust — is written against; there is no
//!    portable way for a plugin on either side of that ABI to verify a foreign window handle's
//!    liveness independently, which is why the CLAP specification states the contract as a
//!    documented caller obligation rather than as something the callee can check.
//!
//! **Why this is sound to accept rather than a real gap.** Every CLAP host implementation (a C/
//! C++ program linking this plugin as a shared library) necessarily makes the identical trust
//! assumption about *every* CLAP plugin it loads, in the other direction — a plugin's `clap_plugin`
//! vtable is itself a set of raw function pointers the host calls with no more verification than
//! this. Rejecting that shared, specification-defined contract here would not make embedding safer,
//! it would make this the one CLAP plugin that cannot embed at all. This is the same class of
//! foreign-ABI trust `namir-platform`'s `thread_priority.rs` already documents for
//! `SetThreadPriority(GetCurrentThread(), ...)` (a valid pseudo-handle "by [Win32's own]
//! documentation", not by anything this process can verify) — a documented platform/protocol
//! contract, trusted because there is no alternative that isn't "do not implement this CLAP
//! extension at all," which would forfeit FR-CLAP-100 entirely.
//!
//! **What this crate does verify, rather than trust blindly.** `is_api_supported`/
//! `get_preferred_api` restrict this plugin to `GuiApiType::WIN32`, non-floating, *before*
//! `set_parent` is ever reachable — so the variant this plugin *expects* to read is fixed.
//! `clack_extensions::gui::Window::raw_window_handle` only returns `Err(HandleError::NotSupported)`
//! for an *unrecognised* `clap_window_t.api` string; for any recognised one (`"win32"`, `"cocoa"`,
//! `"x11"`, ...) it returns `Ok`, regardless of whether that tag matches the `GuiApiType` this
//! plugin negotiated. A host that violates the `set_parent` contract by sending a *recognised but
//! wrong* tag (e.g. `"cocoa"` on Windows) would therefore make `borrow_handle_unchecked()` return
//! `Ok(WindowHandle(AppKit(..)))` here — not an error — and handing that straight to
//! `namir_ui::open_parented` would reach `baseview`'s Windows backend with a non-`Win32` raw
//! handle, which panics (`unsupported parent handle`). `clack_plugin`'s C trampoline catches that
//! panic at the FFI boundary (`PluginWrapper::handle`'s `catch_unwind`) so it cannot become
//! undefined behaviour, but the panic would fire *before* this crate's own `GUI_INVALID_PARENT`
//! diagnostic ever gets a chance to, so the user would see nothing but a silently-failed GUI open.
//! This crate closes that gap itself, below, by matching on `handle.as_raw()` and treating anything
//! other than `RawWindowHandle::Win32` the same way an unrecognised tag is already treated — a
//! pushed notice and an `Err`, never a fallthrough into `open_parented` with the wrong variant.
//!
//! Confined to this one module per D-5.3/NFR-QUAL-070 — `#![allow(unsafe_code)]` below opts only
//! this file back into the one `unsafe` block above out of this crate's `[lints.rust] unsafe_code
//! = "deny"` (`Cargo.toml`), the same "`deny`, not `forbid`, so only a *designated* module can opt
//! back in" shape `namir-platform`'s `denormal.rs` and `thread_priority.rs` already use — two such
//! modules there, one here, and per D-5.3's *Consequence (added M9, 2026-08-08)* none in any test
//! or bench target of either crate.

#![allow(unsafe_code)]

use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, PluginGuiImpl, Window as ClapWindow,
};
use clack_plugin::plugin::PluginError;

use crate::main_thread::NamirMainThread;
use crate::ui_host::ClapUiHost;

/// Fixed editor size, matching `namir_ui::app::default_window_size`'s own opening size
/// (960x640 logical pixels — comfortably above FR-UI-080's 800x600 floor). FR-CLAP-110
/// (host-driven resize, Should) is out of scope this round, matching `spikes/s4-clack-clap`'s own
/// `can_resize() == false`.
const GUI_WIDTH: u32 = 960;
const GUI_HEIGHT: u32 = 640;

impl<'a> PluginGuiImpl for NamirMainThread<'a> {
    fn is_api_supported(&mut self, configuration: GuiConfiguration<'_>) -> bool {
        // Embedded only, Win32 only — matching `spikes/s4-clack-clap`'s validated shape (S-4,
        // `docs/02-architecture.md` §19) and this module's own safety argument above, which
        // depends on this plugin never accepting a `set_parent` call for an API it did not
        // declare support for here.
        configuration.api_type == GuiApiType::WIN32 && !configuration.is_floating
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        Some(GuiConfiguration {
            api_type: GuiApiType::WIN32,
            is_floating: false,
        })
    }

    fn create(&mut self, configuration: GuiConfiguration<'_>) -> Result<(), PluginError> {
        if self.is_api_supported(configuration) {
            Ok(())
        } else {
            Err(PluginError::Message("unsupported GUI API"))
        }
    }

    fn destroy(&mut self) {
        if let Some(window) = self.window.take() {
            window.close();
        }
    }

    fn set_parent(&mut self, window: ClapWindow<'_>) -> Result<(), PluginError> {
        // SAFETY: see this module's doc comment, "D-5.3's written safety argument for
        // `NamirMainThread::set_parent`'s `unsafe` block", in full. Summary: this trusts the CLAP
        // specification's own `set_parent` contract (host supplies a live window matching the
        // `api_type` this plugin already declared support for, valid until `destroy`/host
        // teardown) — the same foreign-ABI trust every CLAP host/plugin pair makes in both
        // directions, and the only alternative to accepting it is not implementing FR-CLAP-100
        // at all. `is_api_supported`/`get_preferred_api` above restrict this plugin to
        // `GuiApiType::WIN32`, non-floating, before this call is ever reachable; the explicit
        // `RawWindowHandle::Win32` match immediately below (not present in the original spike)
        // is what actually enforces that restriction against a host that sent a recognised-but-
        // wrong tag, rather than trusting the host had called `is_api_supported` honestly.
        let handle = unsafe { window.borrow_handle_unchecked() }.map_err(|_| {
            self.shared.inner.push_notice(
                crate::error_codes::GUI_INVALID_PARENT,
                "the host-supplied window handle could not be interpreted",
            );
            PluginError::Message("host window handle unavailable")
        })?;

        // See this function's SAFETY comment and this module's doc comment: a recognised-but-
        // wrong `clap_window_t` tag reaches here as `Ok`, not `Err`, so this variant check is the
        // only thing standing between a spec-violating host and a panic inside `baseview`'s
        // Windows backend (which would otherwise fire before `GUI_INVALID_PARENT` ever gets
        // pushed).
        if !matches!(
            handle.as_raw(),
            raw_window_handle::RawWindowHandle::Win32(_)
        ) {
            self.shared.inner.push_notice(
                crate::error_codes::GUI_INVALID_PARENT,
                "the host supplied a window handle for a different windowing API than the one \
                 this plugin negotiated",
            );
            return Err(PluginError::Message(
                "host window handle does not match the negotiated GUI API",
            ));
        }

        // A prior embedded window, still present because a (spec-violating) host called
        // `set_parent` twice without an intervening `destroy()`: close it explicitly first.
        // `baseview::WindowHandle` has no `Drop` impl (`.close()` is the only teardown path), so
        // simply overwriting `self.window` below would leak the native child window rather than
        // merely leaking Rust memory.
        if let Some(previous) = self.window.take() {
            previous.close();
        }

        // No telemetry reader is passed: `ClapUiHost` fetches whichever one is live, on every
        // frame that needs it, precisely because there may be none yet at editor-open time and
        // because the one that exists now is retired by the next deactivate/reactivate cycle
        // (issue #95).
        let host = ClapUiHost::new(std::sync::Arc::clone(&self.shared.inner));
        self.window = Some(namir_ui::open_parented(&handle, "Namir", host));

        Ok(())
    }

    fn set_transient(&mut self, _window: ClapWindow<'_>) -> Result<(), PluginError> {
        Err(PluginError::Message("floating windows are not supported"))
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: GUI_WIDTH,
            height: GUI_HEIGHT,
        })
    }

    /// **Issue #98: the refusal is reported, not disguised as success.**
    ///
    /// `can_resize()` below is `false` and [`Self::get_size`] returns the same fixed
    /// [`GUI_WIDTH`]x[`GUI_HEIGHT`] whatever a host asks for, so this can only ever *decline*.
    /// CLAP's own `clap_plugin_gui.set_size` is a `bool`-returning call whose contract is "returns
    /// true if the size is (was) accepted"; returning `Ok` for a request nothing acted on tells a
    /// host it may size its parent window to a figure the editor never adopted, and leaves the
    /// editor clipped or adrift in dead space with nothing reporting why.
    ///
    /// The exact current size is accepted, because that request genuinely is satisfied — with
    /// nothing to do. A host that echoes `get_size` back (the documented opening sequence has one
    /// call `set_size` when it "remembers previous session's size") must not be told the plugin
    /// cannot hold the size it just reported.
    ///
    /// **The refusal does not currently reach the host, and that is upstream, not here.**
    /// `clack-extensions` 0.1.1's `set_size` trampoline (`src/gui/plugin.rs:403-412`) is
    /// `PluginWrapper::handle(plugin, |p| Ok(p.main_thread().as_mut().set_size(size))).is_some()`
    /// — it wraps the plugin's whole `Result` as the *success value* and then reports whether the
    /// call panicked, so an `Err` returned here becomes `true` at the C ABI. Every neighbouring
    /// method in that same file gets this right (`set_scale`, `show`, `hide` are all
    /// `Ok(...is_ok())`), so this is a defect in one function rather than the crate's convention.
    /// [`accepts_size`] is factored out so the decision this plugin makes is testable and correct
    /// on the day the answer starts being transmitted, and
    /// `tests/clap_host_gui.rs` carries a live record of the swallowing.
    ///
    /// **Upstream status (issue #144): reported as `prokopyl/clack#101`.** There is now a ticket
    /// to track, but still no released version to move to — crates.io publishes only 0.1.0 and
    /// 0.1.1, neither yanked. The fix is
    /// one line in that trampoline (`Ok(...is_ok())`, as its siblings already read). Nor is the
    /// version *pinned*: `Cargo.toml` declares `"0.1.1"`, i.e. `^0.1.1`, and it is the committed
    /// `Cargo.lock` that holds it — so a `cargo update` that picks up a fixed 0.1.2 is what
    /// retires this, and the host-harness test named above is what says so.
    ///
    /// **Why refuse rather than become resizable.** FR-CLAP-110 (host-driven resize) is a *Should*
    /// this round declares out of scope (see this crate's `lib.rs`), and the fixed 960x640 is a
    /// deliberate, sufficient size: it is comfortably above FR-UI-080's 800x600 floor, and issue
    /// #42's fix — bounding the notice list vertically — was specifically taken so the editor works
    /// *at* that size rather than needing to grow. A real resize would have to reach
    /// `namir_ui::open_parented`'s `baseview` window and re-lay-out the egui frame, which is
    /// `namir-ui` work and a `namir-ui` decision; nothing in this crate can honestly do it today,
    /// and saying so is the whole of what this fix is.
    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        if accepts_size(size) {
            return Ok(());
        }
        Err(PluginError::Message(
            "this editor is a fixed size (can_resize() is false)",
        ))
    }

    fn can_resize(&mut self) -> bool {
        false
    }

    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Whether the editor can adopt `size` — the whole of [`PluginGuiImpl::set_size`]'s decision, split
/// out so it is reachable from a test (constructing a `NamirMainThread` needs a live
/// `HostMainThreadHandle`, which only a real instantiation produces) and so the one-line answer is
/// stated once. See `set_size`'s doc comment for why the answer is what it is, and for why a host
/// currently cannot hear it.
fn accepts_size(size: GuiSize) -> bool {
    size.width == GUI_WIDTH && size.height == GUI_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #98: a fixed-size editor accepts exactly the size it has, and refuses every other —
    /// rather than reporting success for a request that changed nothing.
    #[test]
    fn only_the_size_the_editor_actually_has_is_accepted() {
        assert!(accepts_size(GuiSize {
            width: GUI_WIDTH,
            height: GUI_HEIGHT
        }));
        for refused in [
            (800, 600),
            (1920, 1080),
            (GUI_WIDTH, 480),
            (1280, GUI_HEIGHT),
            (0, 0),
        ] {
            assert!(
                !accepts_size(GuiSize {
                    width: refused.0,
                    height: refused.1
                }),
                "{refused:?} is not a size get_size() will ever report, so accepting it would be \
                 a lie a host then sizes its parent window from"
            );
        }
    }
}
