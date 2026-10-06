//! Emoji, kaomoji and symbol catalogs, search, usage ranking and font coverage. See DESIGN.md §6.

mod catalog;
mod coverage;
mod popular;
#[cfg(test)]
mod render_check;
mod search;
mod usage;

pub use catalog::{Catalog, Entry, Group, Kind, catalog};
pub use coverage::Coverage;
pub use search::search;
pub use usage::Usage;

#[cfg(test)]
mod tests;
