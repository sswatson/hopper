use super::navigation::{
    go_back, invalidate_navigation_requests, rebuild_list_states, refresh_current_level,
    restore_level_selection, select_current_item,
};
use super::*;
use crate::app::state::BUCKET_FORM_FIELDS;

pub(super) async fn handle_event_impl(state: &mut AppState, event: AppEvent) -> Result<()> {
    if event == AppEvent::Quit {
        state.quit();
        return Ok(());
    }

    match state.mode {
        AppMode::Help => {
            handle_help_event(state, event);
            return Ok(());
        }
        AppMode::Input => {
            handle_input_event(state, event);
            return Ok(());
        }
        AppMode::BucketForm => {
            handle_bucket_form_event(state, event);
            return Ok(());
        }
        AppMode::DeleteConfirm => {
            match event {
                AppEvent::ConfirmDelete => operations::confirm_delete(state),
                AppEvent::CancelDelete => {
                    state.delete_target = None;
                    state.mode = AppMode::Normal;
                    state.set_status("Delete cancelled".to_string());
                }
                AppEvent::ToggleDeleteDangerMode => {
                    state.delete_danger_mode = !state.delete_danger_mode;
                }
                _ => {}
            }
            return Ok(());
        }
        AppMode::DeleteDangerConfirm => {
            match event {
                AppEvent::ConfirmEnableDeleteDangerMode => {
                    state.delete_danger_mode = true;
                    state.set_status(
                        "Danger mode ON: deletes skip confirmation (! to turn off)".to_string(),
                    );
                }
                AppEvent::CancelEnableDeleteDangerMode => {
                    state.set_status("Danger mode stays off".to_string());
                }
                _ => return Ok(()),
            }
            state.mode = AppMode::Normal;
            return Ok(());
        }
        AppMode::Search => match event {
            AppEvent::InputChar(c) => {
                state.search_query.push(c);
                apply_search_filter(state);
                preview::note_selection(state);
                return Ok(());
            }
            AppEvent::InputBackspace => {
                state.search_query.pop();
                apply_search_filter(state);
                preview::note_selection(state);
                return Ok(());
            }
            AppEvent::InputSubmit => {
                // Select the highlighted item, then leave search mode.
                let selected = select_current_item(state).await;
                exit_search(state);
                return selected;
            }
            AppEvent::ExitSearch => {
                exit_search(state);
                return Ok(());
            }
            _ => {}
        },
        AppMode::Normal => {}
    }

    handle_global_event(state, event).await?;
    if matches!(state.mode, AppMode::Normal | AppMode::Search) && !state.should_quit {
        preview::note_selection(state);
    }
    Ok(())
}

fn apply_search_filter(state: &mut AppState) {
    invalidate_navigation_requests(state);
    let query = state.search_query.clone();
    if let Some(list_state) = state.active_list_state_mut() {
        list_state.filter(&query);
    }
    state.preview_scroll = 0;
}

fn exit_search(state: &mut AppState) {
    state.mode = AppMode::Normal;
    state.search_query.clear();
    if let Some(list_state) = state.active_list_state_mut() {
        list_state.filter("");
    }
}

fn handle_help_event(state: &mut AppState, event: AppEvent) {
    match event {
        AppEvent::MoveUp => state.help_scroll = state.help_scroll.saturating_sub(1),
        AppEvent::MoveDown => state.help_scroll = state.help_scroll.saturating_add(1),
        AppEvent::PageUp => state.help_scroll = state.help_scroll.saturating_sub(10),
        AppEvent::PageDown => state.help_scroll = state.help_scroll.saturating_add(10),
        AppEvent::GoToTop => state.help_scroll = 0,
        AppEvent::GoToBottom => state.help_scroll = usize::MAX / 2,
        AppEvent::ToggleHelp => {
            state.help_scroll = 0;
            state.mode = AppMode::Normal;
        }
        _ => {}
    }
}

fn edit_buffer(buffer: &mut String, event: &AppEvent) {
    match event {
        AppEvent::InputChar(c) => buffer.push(*c),
        AppEvent::InputBackspace => {
            buffer.pop();
        }
        AppEvent::InputClear => buffer.clear(),
        AppEvent::InputDeleteWord => {
            // Delete back to (and including) the previous separator, like a
            // shell's Ctrl+W but treating `/` as a word boundary for paths.
            let trimmed_len = buffer.trim_end_matches(['/', ' ']).len();
            buffer.truncate(trimmed_len);
            let cut = buffer.rfind(['/', ' ']).map(|idx| idx + 1).unwrap_or(0);
            buffer.truncate(cut);
        }
        _ => {}
    }
}

fn handle_input_event(state: &mut AppState, event: AppEvent) {
    match event {
        AppEvent::InputCancel => {
            state.text_input = None;
            state.mode = AppMode::Normal;
        }
        AppEvent::InputSubmit => {
            if let Err(err) = operations::submit_text_input(state) {
                state.set_status(format!("{:#}", err));
            }
        }
        AppEvent::InputComplete => {
            if let Some(input) = state.text_input.as_mut()
                && input.purpose.is_local_path()
            {
                let (buffer, hint) = operations::complete_local_path(&input.buffer);
                input.buffer = buffer;
                input.hint = hint;
            }
        }
        other => {
            if let Some(input) = state.text_input.as_mut() {
                edit_buffer(&mut input.buffer, &other);
                input.hint = None;
            }
        }
    }
}

fn handle_bucket_form_event(state: &mut AppState, event: AppEvent) {
    let field_count = BUCKET_FORM_FIELDS.len();
    match event {
        AppEvent::InputCancel => state.mode = AppMode::Normal,
        AppEvent::MoveUp => {
            state.bucket_form.selected =
                (state.bucket_form.selected + field_count - 1) % field_count;
        }
        AppEvent::MoveDown => {
            state.bucket_form.selected = (state.bucket_form.selected + 1) % field_count;
        }
        AppEvent::InputSubmit => {
            if let Err(err) = operations::submit_bucket_form(state) {
                state.set_status(format!("{:#}", err));
            }
        }
        other => {
            let selected = state.bucket_form.selected;
            edit_buffer(&mut state.bucket_form.values[selected], &other);
        }
    }
}

// Shared event handling for Normal mode (and the navigation keys that
// search mode lets through).
async fn handle_global_event(state: &mut AppState, event: AppEvent) -> Result<()> {
    match event {
        AppEvent::MoveUp => {
            invalidate_navigation_requests(state);
            if let Some(list_state) = state.active_list_state_mut() {
                list_state.select_previous();
            }
            state.preview_scroll = 0;
        }
        AppEvent::MoveDown => {
            invalidate_navigation_requests(state);
            if let Some(list_state) = state.active_list_state_mut() {
                list_state.select_next();
            }
            state.preview_scroll = 0;
        }
        AppEvent::MoveLeft => {
            invalidate_navigation_requests(state);
            if state.active_pane > 0 {
                state.active_pane -= 1;
                state.clear_status();
            } else if state.window_start > 0 {
                // At leftmost visible pane, slide window left
                state.window_start -= 1;
                rebuild_list_states(state);
                state.clear_status();
            }
            state.preview_scroll = 0;
        }
        AppEvent::MoveRight => {
            invalidate_navigation_requests(state);
            if state.active_pane < state.list_states.len().saturating_sub(1) {
                restore_level_selection(state, state.active_pane);
                state.active_pane += 1;
                state.clear_status();
            } else {
                let max_panes = state.visible_pane_count();
                let total_levels = state.navigation.levels_len();
                let max_window_start = total_levels.saturating_sub(max_panes);

                if state.window_start < max_window_start {
                    // At rightmost visible pane, slide window right
                    restore_level_selection(state, state.active_pane);
                    state.window_start += 1;
                    rebuild_list_states(state);
                    state.active_pane = state.list_states.len().saturating_sub(1);
                    state.clear_status();
                }
            }
            state.preview_scroll = 0;
        }
        AppEvent::PageUp => {
            invalidate_navigation_requests(state);
            if let Some(list_state) = state.active_list_state_mut() {
                list_state.page_up(10);
            }
            state.preview_scroll = 0;
        }
        AppEvent::PageDown => {
            invalidate_navigation_requests(state);
            if let Some(list_state) = state.active_list_state_mut() {
                list_state.page_down(10);
            }
            state.preview_scroll = 0;
        }
        AppEvent::GoToTop => {
            invalidate_navigation_requests(state);
            if let Some(list_state) = state.active_list_state_mut() {
                list_state.select_first();
            }
            state.preview_scroll = 0;
        }
        AppEvent::GoToBottom => {
            invalidate_navigation_requests(state);
            if let Some(list_state) = state.active_list_state_mut() {
                list_state.select_last();
            }
            state.preview_scroll = 0;
        }
        AppEvent::NextPane => {
            invalidate_navigation_requests(state);
            if state.active_pane < state.list_states.len().saturating_sub(1) {
                state.active_pane += 1;
                state.clear_status();
            }
            state.preview_scroll = 0;
        }
        AppEvent::PreviousPane => {
            invalidate_navigation_requests(state);
            if state.active_pane > 0 {
                state.active_pane -= 1;
                state.clear_status();
            }
            state.preview_scroll = 0;
        }
        AppEvent::SelectItem => {
            let previous_status = state.status_message.clone();
            select_current_item(state).await?;
            if state.status_message == previous_status {
                state.clear_status();
            }
            state.preview_scroll = 0;
        }
        AppEvent::GoBack => {
            invalidate_navigation_requests(state);
            go_back(state);
            state.clear_status();
            state.preview_scroll = 0;
        }
        AppEvent::Refresh => refresh_current_level(state),
        AppEvent::ToggleHelp => {
            state.help_scroll = 0;
            state.mode = AppMode::Help;
        }
        AppEvent::StartSearch => {
            state.mode = AppMode::Search;
            state.search_query.clear();
        }
        AppEvent::PreviewScrollUp => {
            state.preview_scroll = state.preview_scroll.saturating_sub(1);
        }
        AppEvent::PreviewScrollDown => {
            state.preview_scroll = state.preview_scroll.saturating_add(1);
        }
        AppEvent::CopyPath => operations::copy_path(state),
        AppEvent::Download => operations::start_download(state),
        AppEvent::Upload => operations::start_upload(state),
        AppEvent::CreateItem => operations::start_create(state),
        AppEvent::DeleteItem => operations::start_delete(state),
        AppEvent::RenameItem => operations::start_rename(state),
        AppEvent::CutSelection => operations::cut_selection(state),
        AppEvent::PasteSelection => operations::paste_selection(state),
        AppEvent::OpenInBrowser => operations::open_in_browser(state),
        AppEvent::FetchPrefixSize => preview::fetch_prefix_size(state),
        AppEvent::ToggleSelect => {
            if let Some(item) = state.selected_item().cloned() {
                if item.item_type != ItemType::LoadMore {
                    let path = item.full_path();
                    if state.selected_items.remove(&path).is_none() {
                        state.selected_items.insert(path, item);
                    }
                }
                if let Some(list_state) = state.active_list_state_mut()
                    && list_state.selected + 1 < list_state.len()
                {
                    list_state.selected += 1;
                }
            }
        }
        AppEvent::ToggleDeleteDangerMode => {
            if state.delete_danger_mode {
                state.delete_danger_mode = false;
                state.set_status("Danger mode OFF: deletes ask for confirmation".to_string());
            } else {
                state.mode = AppMode::DeleteDangerConfirm;
            }
        }
        AppEvent::ExitSearch
        | AppEvent::ConfirmDelete
        | AppEvent::CancelDelete
        | AppEvent::ConfirmEnableDeleteDangerMode
        | AppEvent::CancelEnableDeleteDangerMode
        | AppEvent::InputChar(_)
        | AppEvent::InputBackspace
        | AppEvent::InputDeleteWord
        | AppEvent::InputClear
        | AppEvent::InputComplete
        | AppEvent::InputSubmit
        | AppEvent::InputCancel
        | AppEvent::Quit => {
            // Handled by the mode-specific branches above.
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::edit_buffer;
    use crate::app::events::AppEvent;

    #[test]
    fn ctrl_w_deletes_one_path_segment() {
        let mut buffer = "/home/me/data/".to_string();
        edit_buffer(&mut buffer, &AppEvent::InputDeleteWord);
        assert_eq!(buffer, "/home/me/");
        edit_buffer(&mut buffer, &AppEvent::InputDeleteWord);
        assert_eq!(buffer, "/home/");
        let mut single = "file".to_string();
        edit_buffer(&mut single, &AppEvent::InputDeleteWord);
        assert_eq!(single, "");
    }
}
