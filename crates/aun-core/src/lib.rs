//! Aun's engine: discovery, pairing, encrypted links, clipboard and file transfer. No UI.

pub mod clip;
pub mod discovery;
pub mod node;
pub mod store;
pub mod wire;

pub use node::{Config, Node, Note, PeerView};
pub use store::{Id, hex};
