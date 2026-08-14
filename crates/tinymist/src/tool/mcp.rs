//! The machine face: what an agent talks to.
//!
//! Two halves of the same protocol. `tools` is what one document server
//! answers at its own `/m/` — the tools that read and write the annotations of
//! the documents it holds. `dispatch` is `talimist mcp`, the single address an
//! agent is told about: it holds no documents itself and forwards each call to
//! whichever server holds the one being asked about, starting one if none is.
//!
//! An agent therefore needs one address for every document on the machine, and
//! never has to be told a port.

pub mod dispatch;
pub mod tools;

pub use dispatch::HUB_PORT;
