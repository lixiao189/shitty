//! VT grid. The UI reads a [`GridView`]; only this module mutates libghostty state.

mod term;
mod view;

pub(crate) use term::TerminalGrid;
pub(crate) use view::{
    CellInfo, CellPos, CellRun, CursorInfo, CursorShape, GridView, display_cells,
};
