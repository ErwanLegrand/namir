//! CLAP's `params` extension (`clack_extensions::params`) — required infrastructure for
//! FR-PARAM-030 ("parameter changes shall be accepted from the UI, CLAP automation, and preset
//! loading") and the concrete mechanism FR-CLAP-060 (host-driven bypass) is built on: see this
//! module's `global.bypass` handling in [`param_info`], which marks that one `REGISTRY` entry
//! with CLAP's own [`ParamInfoFlags::IS_BYPASS`] — "used to merge the plugin and host bypass
//! button" per `clack_extensions::params`'s own module doc comment — rather than inventing a
//! separate ad hoc flag. Once marked, a host's own bypass button sends an ordinary
//! `ParamValueEvent` on `global.bypass`'s id, which reaches `Chain::apply`/`set_global_bypass`
//! through exactly the same path every other automated parameter does
//! (`crate::audio`'s `apply_param_direct`) — D-10.4 (this session's own prerequisite decision)
//! is what made this possible at all: before it, global bypass had no `ParamDescriptor`, so there
//! was nothing here to flag.

use std::ffi::CStr;

use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams,
};
use clack_plugin::events::io::{InputEvents, OutputEvents};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::utils::{ClapId, Cookie};
use namir_engine::{ParamChange, ParamId as EngineParamId};
use namir_params::global::GLOBAL_BYPASS;
use namir_params::{ParamDescriptor, ParamKind, REGISTRY};

use crate::audio::NamirAudioProcessor;
use crate::main_thread::NamirMainThread;

fn descriptor_by_id(id: ClapId) -> Option<&'static ParamDescriptor> {
    REGISTRY.iter().find(|d| d.id.0 == id.get())
}

fn param_info(descriptor: &'static ParamDescriptor) -> ParamInfo<'static> {
    let mut flags = ParamInfoFlags::IS_AUTOMATABLE;
    let (min_value, max_value, default_value) = match descriptor.kind {
        ParamKind::Continuous { min, max, default } => (min as f64, max as f64, default as f64),
        ParamKind::Stepped {
            values,
            default_index,
        } => {
            flags |= ParamInfoFlags::IS_STEPPED;
            (
                0.0,
                (values.len().saturating_sub(1)) as f64,
                default_index.0 as f64,
            )
        }
    };
    // FR-CLAP-060: the one descriptor CLAP's own bypass convention applies to.
    if descriptor.key == GLOBAL_BYPASS.key {
        flags |= ParamInfoFlags::IS_BYPASS;
    }
    ParamInfo {
        id: ClapId::new(descriptor.id.0),
        flags,
        cookie: Cookie::default(),
        name: descriptor.name.as_bytes(),
        module: b"",
        min_value,
        max_value,
        default_value,
    }
}

/// FR-UI-040's parse direction, reimplemented here rather than depending on `namir-ui::format`
/// (a private module of that crate, not re-exported — see its own `lib.rs`) — small enough
/// (`ParamKind`'s own two shapes) not to warrant changing that crate's public surface for one
/// caller.
fn parse_text_to_value(descriptor: &ParamDescriptor, text: &str) -> Option<f64> {
    let trimmed = text.trim();
    match descriptor.kind {
        ParamKind::Continuous { min, max, .. } => trimmed
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(min as f64, max as f64)),
        ParamKind::Stepped { values, .. } => {
            if let Some(index) = values.iter().position(|v| v.eq_ignore_ascii_case(trimmed)) {
                return Some(index as f64);
            }
            let max_index = values.len().saturating_sub(1) as f64;
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(|v| v.round().clamp(0.0, max_index))
        }
    }
}

/// Applies every `ParamValue` event in `input` to `apply`, mirroring it into the `ParamMirror` via
/// `mirror` — shared between the main-thread and audio-processor `flush` implementations below.
fn apply_flush_events(
    input: &InputEvents,
    mut apply: impl FnMut(ParamChange),
    mirror: &crate::param_mirror::ParamMirror,
) {
    for event in input.iter() {
        if let Some(CoreEventSpace::ParamValue(ev)) = event.as_core_event()
            && let Some(id) = ev.param_id()
        {
            let value = ev.value() as f32;
            apply(ParamChange {
                id: EngineParamId(id.get()),
                value,
            });
            mirror.set_by_id(id.get(), value);
        }
    }
}

impl<'a> PluginMainThreadParams for NamirMainThread<'a> {
    fn count(&mut self) -> u32 {
        REGISTRY.len() as u32
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        if let Some(descriptor) = REGISTRY.get(param_index as usize) {
            info.set(&param_info(descriptor));
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        self.shared
            .inner
            .params
            .get_by_id(param_id.get())
            .map(|v| v as f64)
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        use std::fmt::Write;
        if let Some(descriptor) = descriptor_by_id(param_id) {
            write!(writer, "{}", descriptor.format_value(value as f32))
        } else {
            Err(std::fmt::Error)
        }
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let descriptor = descriptor_by_id(param_id)?;
        let text = text.to_str().ok()?;
        parse_text_to_value(descriptor, text)
    }

    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        _output_parameter_changes: &mut OutputEvents,
    ) {
        // Inactive (no live engine): update only the mirror, which the *next* `activate()`'s
        // replay (`crate::audio`) will push onto a fresh engine. See `crate::shared`'s
        // `SharedInner::with_instance` — a `None` instance here is not an error, just "not yet
        // activated", handled the same way `try_submit_param` degrades when abandoned.
        let mirror = &self.shared.inner.params;
        apply_flush_events(
            input_parameter_changes,
            |change| {
                self.shared.inner.with_instance(|instance| {
                    let _ = instance.try_submit_param(change);
                });
            },
            mirror,
        );
    }
}

impl<'a> PluginAudioProcessorParams for NamirAudioProcessor<'a> {
    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        _output_parameter_changes: &mut OutputEvents,
    ) {
        // Active, but `process()` was not called this cycle -- still the audio thread (per
        // `clack_plugin`'s own thread-model doc comment on `PluginAudioProcessorParams`), so this
        // uses the same direct-apply path `process()` itself uses, not the ring.
        for event in input_parameter_changes.iter() {
            if let Some(CoreEventSpace::ParamValue(ev)) = event.as_core_event()
                && let Some(id) = ev.param_id()
            {
                self.apply_direct_and_mirror(EngineParamId(id.get()), ev.value() as f32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // trace-partial: FR-CLAP-060
    // uncovered: FR-CLAP-060 — both "sample-accurate" and "click-free" are unspanned, the tagged
    // uncovered: test asserting only that the bypass descriptor carries the IS_BYPASS flag and
    // uncovered: processing no audio; sample-accuracy is contradicted by apply_automation, which is
    // uncovered: called once before the block and applies every ParamValue event immediately
    // uncovered: without reading the event header's sample offset; closes M9b
    #[test]
    fn global_bypass_param_info_carries_the_is_bypass_flag() {
        let info = param_info(&GLOBAL_BYPASS);
        assert!(info.flags.contains(ParamInfoFlags::IS_BYPASS));
        assert!(info.flags.contains(ParamInfoFlags::IS_STEPPED));
        assert_eq!(info.min_value, 0.0);
        assert_eq!(info.max_value, 1.0);
    }

    #[test]
    fn a_continuous_descriptor_carries_no_bypass_or_stepped_flag() {
        let info = param_info(&namir_params::stages::trim::GAIN_DB);
        assert!(!info.flags.contains(ParamInfoFlags::IS_BYPASS));
        assert!(!info.flags.contains(ParamInfoFlags::IS_STEPPED));
    }

    #[test]
    fn parse_text_to_value_matches_namir_ui_formats_semantics() {
        let d = &namir_params::stages::trim::GAIN_DB;
        assert_eq!(parse_text_to_value(d, "6.0"), Some(6.0));
        assert_eq!(parse_text_to_value(d, "loud"), None);
    }

    #[test]
    fn parse_text_to_value_accepts_a_named_stepped_value_case_insensitively() {
        let d = &namir_params::stages::gate::ENABLED;
        assert_eq!(parse_text_to_value(d, "on"), Some(1.0));
    }

    #[test]
    fn descriptor_by_id_finds_a_known_registry_entry() {
        let id = ClapId::new(namir_params::stages::trim::GAIN_DB.id.0);
        assert_eq!(
            descriptor_by_id(id).map(|d| d.key),
            Some(namir_params::stages::trim::GAIN_DB.key)
        );
    }

    #[test]
    fn descriptor_by_id_returns_none_for_an_unknown_id() {
        assert!(descriptor_by_id(ClapId::new(0xFFFF_FFFE)).is_none());
    }
}
