//! CLAP's `audio-ports` extension (FR-CLAP-030). Declares exactly one stereo input port and one
//! stereo output port, in place (the input and output buffers may be the same memory — our
//! engine processes in place, `StageIo`'s own long-standing contract).
//!
//! **Scope reduction, stated plainly rather than glossed over:** FR-CLAP-030 asks for port
//! configurations "corresponding to FR-CHAIN-060", which names three (Mono, Mono→stereo, Stereo).
//! This round declares Stereo only — the configuration most hosts default a guitar/bass track to
//! — and does not implement the `audio-ports-config` extension CLAP would need to let a host pick
//! among several declared configurations at load time. FR-CLAP-030's "correctly negotiate the
//! configuration the host requests" is satisfied for the one configuration this plugin declares
//! (a host that queries `count`/`get` sees exactly what it gets, correctly, every time); it is
//! *not* satisfied in the sense of offering Mono or Mono→stereo as alternatives. Recorded here,
//! and in `docs/manual-tests/fr-clap-030-audio-ports-negotiation.md`, as a scope decision to
//! revisit rather than a silent gap — raised as `docs/03-implementation-roadmap.md` §15 item 9,
//! due before M8.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPortsImpl,
};
use clack_plugin::utils::ClapId;

use crate::main_thread::NamirMainThread;

/// The one port id both the input and output port use — legal per
/// `AudioPortInfo::id`'s own doc comment ("IDs are allowed to match across directions"), and what
/// `in_place_pair` on each port references to declare the pairing.
const STEREO_PORT_ID: u32 = 0;

impl<'a> PluginAudioPortsImpl for NamirMainThread<'a> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(STEREO_PORT_ID),
            name: if is_input {
                b"Stereo In"
            } else {
                b"Stereo Out"
            },
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: Some(ClapId::new(STEREO_PORT_ID)),
        });
    }
}
