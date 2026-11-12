//! Shared types, bounds, events and the feed ring buffer for all NME contracts.
pub mod events;
pub mod ext;
pub mod feed;
pub mod math;
pub mod params;
pub mod ticker;

pub use events::emit;
pub use feed::{Feed, FeedItem};
pub use math::mul_div;
pub use params::*;
pub use ticker::{normalize_ticker, validate_account_len};
