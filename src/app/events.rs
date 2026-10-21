use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    // Navigation events
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    PageUp,
    PageDown,
    GoToTop,
    GoToBottom,
    SelectItem,
    GoBack,

    // Tab navigation
    NextPane,
    PreviousPane,

    // Actions
    Refresh,
    ToggleHelp,
    StartSearch,
    PreviewScrollUp,
    PreviewScrollDown,
    CopyPath,
    Download,
    Upload,
    CreateItem,
    DeleteItem,
    RenameItem,
    ToggleSelect,
    CutSelection,
    PasteSelection,
    OpenInBrowser,
    ViewObject,
    FetchPrefixSize,
    ToggleDeleteDangerMode,

    // Mode changes
    ExitSearch,
    ConfirmDelete,
    CancelDelete,
    ConfirmEnableDeleteDangerMode,
    CancelEnableDeleteDangerMode,

    // Text input
    InputChar(char),
    InputBackspace,
    InputDeleteWord,
    InputClear,
    InputComplete,
    InputSubmit,
    InputCancel,

    Quit,
}

fn is_text_entry(modifiers: KeyModifiers) -> bool {
    modifiers == KeyModifiers::NONE || modifiers == KeyModifiers::SHIFT
}

/// Search keeps list navigation available while typing.
fn search_edit_event(key: KeyEvent) -> Option<AppEvent> {
    match key.code {
        KeyCode::Up => Some(AppEvent::MoveUp),
        KeyCode::Down => Some(AppEvent::MoveDown),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::PageUp)
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::PageDown)
        }
        KeyCode::Char(c) if is_text_entry(key.modifiers) => Some(AppEvent::InputChar(c)),
        KeyCode::Backspace => Some(AppEvent::InputBackspace),
        _ => None,
    }
}

/// Shared editing keys for prompts and form fields.
fn text_entry_event(key: KeyEvent) -> Option<AppEvent> {
    match key.code {
        KeyCode::Enter => Some(AppEvent::InputSubmit),
        KeyCode::Esc => Some(AppEvent::InputCancel),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::InputClear)
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppEvent::InputDeleteWord)
        }
        KeyCode::Char(c) if is_text_entry(key.modifiers) => Some(AppEvent::InputChar(c)),
        KeyCode::Backspace => Some(AppEvent::InputBackspace),
        _ => None,
    }
}

impl AppEvent {
    pub fn from_key_event(key: KeyEvent, mode: crate::app::state::AppMode) -> Option<Self> {
        use crate::app::state::AppMode;

        match mode {
            AppMode::Search => match key.code {
                KeyCode::Esc => Some(AppEvent::ExitSearch),
                // Selects the highlighted item and then leaves search mode.
                KeyCode::Enter => Some(AppEvent::InputSubmit),
                _ => search_edit_event(key),
            },
            AppMode::Help => {
                // In help mode, support scrolling.
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::MoveUp),
                    KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::MoveDown),
                    KeyCode::PageUp => Some(AppEvent::PageUp),
                    KeyCode::PageDown => Some(AppEvent::PageDown),
                    KeyCode::Home => Some(AppEvent::GoToTop),
                    KeyCode::End => Some(AppEvent::GoToBottom),
                    KeyCode::Char('?') | KeyCode::Esc => Some(AppEvent::ToggleHelp),
                    _ => None,
                }
            }
            AppMode::Input => match key.code {
                KeyCode::Tab => Some(AppEvent::InputComplete),
                _ => text_entry_event(key),
            },
            AppMode::BucketForm => match key.code {
                KeyCode::Up | KeyCode::BackTab => Some(AppEvent::MoveUp),
                KeyCode::Down | KeyCode::Tab => Some(AppEvent::MoveDown),
                _ => text_entry_event(key),
            },
            AppMode::DeleteConfirm => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    Some(AppEvent::ConfirmDelete)
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    Some(AppEvent::CancelDelete)
                }
                KeyCode::Char('!') => Some(AppEvent::ToggleDeleteDangerMode),
                _ => None,
            },
            AppMode::DeleteDangerConfirm => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    Some(AppEvent::ConfirmEnableDeleteDangerMode)
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    Some(AppEvent::CancelEnableDeleteDangerMode)
                }
                _ => None,
            },
            AppMode::Normal => {
                match (key.code, key.modifiers) {
                    // Quit
                    (KeyCode::Char('q'), KeyModifiers::NONE) => Some(AppEvent::Quit),
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(AppEvent::Quit),

                    // Vim navigation
                    (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                        Some(AppEvent::MoveDown)
                    }
                    (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                        Some(AppEvent::MoveUp)
                    }
                    (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _) => {
                        Some(AppEvent::MoveLeft)
                    }
                    (KeyCode::Char('l'), KeyModifiers::NONE) | (KeyCode::Right, _) => {
                        Some(AppEvent::MoveRight)
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                        Some(AppEvent::GoToTop)
                    }
                    (KeyCode::Char('G'), KeyModifiers::SHIFT) | (KeyCode::End, _) => {
                        Some(AppEvent::GoToBottom)
                    }
                    (KeyCode::Char('u'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
                        Some(AppEvent::PageUp)
                    }
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                        Some(AppEvent::PageDown)
                    }

                    // Selection
                    (KeyCode::Enter, _) => Some(AppEvent::SelectItem),
                    (KeyCode::Char(' '), KeyModifiers::NONE) => Some(AppEvent::ToggleSelect),
                    (KeyCode::Backspace, _) | (KeyCode::Esc, _) => Some(AppEvent::GoBack),

                    // Tab navigation
                    (KeyCode::Tab, KeyModifiers::NONE) => Some(AppEvent::NextPane),
                    (KeyCode::BackTab, _) | (KeyCode::Tab, KeyModifiers::SHIFT) => {
                        Some(AppEvent::PreviousPane)
                    }

                    // Actions
                    (KeyCode::Char('R'), _) => Some(AppEvent::Refresh),
                    (KeyCode::Char('?'), _) => Some(AppEvent::ToggleHelp),
                    (KeyCode::Char('/'), KeyModifiers::NONE) => Some(AppEvent::StartSearch),
                    (KeyCode::Char('['), KeyModifiers::NONE) => Some(AppEvent::PreviewScrollUp),
                    (KeyCode::Char(']'), KeyModifiers::NONE) => Some(AppEvent::PreviewScrollDown),
                    (KeyCode::Char('y'), KeyModifiers::NONE) => Some(AppEvent::CopyPath),
                    (KeyCode::Char('D'), _) => Some(AppEvent::Download),
                    (KeyCode::Char('U'), _) => Some(AppEvent::Upload),
                    (KeyCode::Char('a'), KeyModifiers::NONE) => Some(AppEvent::CreateItem),
                    (KeyCode::Char('d'), KeyModifiers::NONE) => Some(AppEvent::DeleteItem),
                    (KeyCode::Char('!'), _) => Some(AppEvent::ToggleDeleteDangerMode),
                    (KeyCode::Char('r'), KeyModifiers::NONE) => Some(AppEvent::RenameItem),
                    (KeyCode::Char('z'), KeyModifiers::NONE) => Some(AppEvent::FetchPrefixSize),
                    (KeyCode::Char('x'), KeyModifiers::NONE) => Some(AppEvent::CutSelection),
                    (KeyCode::Char('p'), KeyModifiers::NONE) => Some(AppEvent::PasteSelection),
                    (KeyCode::Char('o'), KeyModifiers::NONE) => Some(AppEvent::OpenInBrowser),
                    (KeyCode::Char('v'), KeyModifiers::NONE) => Some(AppEvent::ViewObject),

                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppMode;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn normal_mode_maps_core_navigation_and_actions() {
        let cases = [
            (KeyCode::Char('q'), KeyModifiers::NONE, AppEvent::Quit),
            (KeyCode::Char('j'), KeyModifiers::NONE, AppEvent::MoveDown),
            (KeyCode::Char('h'), KeyModifiers::NONE, AppEvent::MoveLeft),
            (
                KeyCode::Char('G'),
                KeyModifiers::SHIFT,
                AppEvent::GoToBottom,
            ),
            (KeyCode::Char('g'), KeyModifiers::CONTROL, AppEvent::GoToTop),
            (
                KeyCode::Char('d'),
                KeyModifiers::CONTROL,
                AppEvent::PageDown,
            ),
            (KeyCode::Char('d'), KeyModifiers::NONE, AppEvent::DeleteItem),
            (KeyCode::Char('D'), KeyModifiers::SHIFT, AppEvent::Download),
            (KeyCode::Char('U'), KeyModifiers::SHIFT, AppEvent::Upload),
            (KeyCode::Char('R'), KeyModifiers::SHIFT, AppEvent::Refresh),
            (KeyCode::Char('y'), KeyModifiers::NONE, AppEvent::CopyPath),
            (
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                AppEvent::ToggleSelect,
            ),
            (KeyCode::Backspace, KeyModifiers::NONE, AppEvent::GoBack),
            (KeyCode::Esc, KeyModifiers::NONE, AppEvent::GoBack),
        ];
        for (code, modifiers, expected) in cases {
            assert_eq!(
                AppEvent::from_key_event(key(code, modifiers), AppMode::Normal),
                Some(expected),
                "{:?} {:?}",
                code,
                modifiers
            );
        }
    }

    #[test]
    fn search_mode_types_letters_that_are_shortcuts_in_normal_mode() {
        assert_eq!(
            AppEvent::from_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE), AppMode::Search),
            Some(AppEvent::InputChar('q'))
        );
        assert_eq!(
            AppEvent::from_key_event(key(KeyCode::Enter, KeyModifiers::NONE), AppMode::Search),
            Some(AppEvent::InputSubmit)
        );
    }

    #[test]
    fn input_mode_tab_completes_and_bucket_form_tab_moves_field() {
        assert_eq!(
            AppEvent::from_key_event(key(KeyCode::Tab, KeyModifiers::NONE), AppMode::Input),
            Some(AppEvent::InputComplete)
        );
        assert_eq!(
            AppEvent::from_key_event(key(KeyCode::Tab, KeyModifiers::NONE), AppMode::BucketForm),
            Some(AppEvent::MoveDown)
        );
    }
}
