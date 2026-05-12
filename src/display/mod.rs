//! Display layer: HTML routes, MCP tool handlers, maud templates.
//!
//! Dumb and wide. Iterate sources, ask each for its health, render the
//! grid. Business logic lives in `process`, not here. See issue #92 for
//! the design.

pub mod routes;
