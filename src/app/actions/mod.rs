mod dispatch;
mod editing;
mod navigation;
mod operations;
mod preview;
mod terminal;
#[cfg(test)]
mod tests;
mod viewing;

use crate::app::events::AppEvent;
use crate::app::state::*;
use crate::client::*;
use anyhow::{Result, anyhow};
use std::sync::Arc;

pub use editing::cleanup_temp_files;
pub use navigation::item_parent_location;

/// Handles one event. Action failures are reported in the status bar rather
/// than propagated, so a failed S3 call never takes the TUI down.
pub async fn handle_event(state: &mut AppState, event: AppEvent) -> Result<()> {
    if let Err(err) = dispatch::handle_event_impl(state, event).await {
        state.set_status(format!("Error: {:#}", err));
    }
    Ok(())
}

pub fn initialize_app(state: &mut AppState) {
    navigation::initialize_app_impl(state);
}

/// Applies results from background tasks; called once per event-loop tick.
pub async fn poll_background_tasks(state: &mut AppState) -> Result<()> {
    if let Err(err) = navigation::poll_background_tasks_impl(state).await {
        state.set_status(format!("Error: {:#}", err));
    }
    Ok(())
}
