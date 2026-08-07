//! Shared vocabulary types with no logic of their own beyond what's intrinsic to the type
//! (D-5.1): sample rate, channel configuration, dB/linear conversion, content hash, and the
//! error-catalogue framework (D-16.1).

mod channel_config;
mod content_hash;
mod error;
mod gain;
mod limits;
mod sample_rate;

pub use channel_config::ChannelConfig;
pub use content_hash::{ContentHash, ContentHashParseError, ContentHasher};
pub use error::{ErrorCode, Severity, assert_unique_ids};
pub use gain::{db_to_linear, linear_to_db};
pub use limits::MAX_FILE_BYTES;
pub use sample_rate::SampleRate;
