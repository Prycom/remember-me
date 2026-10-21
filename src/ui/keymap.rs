// Клавиши → действия. Раскладка нормализуется по физическим позициям:
// русская буква работает как латинская на той же клавише.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::action::{Action, Focus, Pane};
use crate::app::{App, Screen};

pub fn action_for(k: &KeyEvent, app: &App) -> Option<Action> {
    let vox = app.engine.params.vox_mode();
    if k.kind == KeyEventKind::Release {
        return if k.code == KeyCode::Char(' ') {
            Some(Action::Ptt(false))
        } else {
            None
        };
    }

    if k.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Some(Action::Quit);
    }
    if app.overlay == Some(crate::action::Overlay::Media) {
        return match k.code {
            KeyCode::Char(' ') if !vox => Some(Action::Ptt(true)),
            KeyCode::Char('p' | 'P' | 'з' | 'З') if k.kind == KeyEventKind::Press => {
                Some(Action::MediaPause)
            }
            KeyCode::Esc => Some(Action::Cancel),
            KeyCode::Char('o' | 'O' | 'щ' | 'Щ') if k.kind == KeyEventKind::Press => {
                Some(Action::OpenExternal)
            }
            KeyCode::Char(' ') | KeyCode::Enter if k.kind == KeyEventKind::Press => {
                Some(Action::MediaPause)
            }
            KeyCode::Up => Some(Action::Up),
            KeyCode::Down => Some(Action::Down),
            KeyCode::Left => Some(Action::Left),
            KeyCode::Right => Some(Action::Right),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::Home => Some(Action::Home),
            KeyCode::End => Some(Action::End),
            _ => None,
        };
    }
    if app.overlay.is_none()
        && k.kind == KeyEventKind::Press
        && (matches!(k.code, KeyCode::Char('v' | 'V'))
            && k.modifiers.contains(KeyModifiers::CONTROL)
            || k.code == KeyCode::Insert
                && k.modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL))
    {
        return Some(Action::ClipboardPaste);
    }
    if app.overlay == Some(crate::action::Overlay::Profile) && k.kind == KeyEventKind::Press {
        match k.code {
            KeyCode::Left => return Some(Action::Left),
            KeyCode::Right => return Some(Action::Right),
            KeyCode::Char('v' | 'V' | 'м' | 'М') => return Some(Action::CycleAvatars),
            KeyCode::Char('a' | 'A' | 'ф' | 'Ф') if app.call_sel == 0 => {
                return Some(Action::EditAvatar);
            }
            _ => {}
        }
    }
    if app.overlay.is_some() {
        return match k.code {
            KeyCode::Esc | KeyCode::Char('?') => Some(Action::Cancel),
            KeyCode::Enter
            | KeyCode::Char('y')
            | KeyCode::Char('Y')
            | KeyCode::Char('н')
            | KeyCode::Char('Н')
                if k.kind != KeyEventKind::Repeat =>
            {
                Some(Action::Confirm)
            }
            _ => None,
        };
    }

    if app.file_input.is_some() {
        return match k.code {
            KeyCode::Backspace => Some(Action::Backspace),
            KeyCode::Enter if k.kind == KeyEventKind::Press => Some(Action::Confirm),
            KeyCode::Esc if k.kind == KeyEventKind::Press => Some(Action::Cancel),
            KeyCode::Char(c) if plain(k.modifiers) => Some(Action::Insert(typed(c, k.modifiers))),
            _ => None,
        };
    }
    if k.kind == KeyEventKind::Press {
        match k.code {
            KeyCode::F(2) => return Some(Action::OpenSettings),
            KeyCode::F(3) if app.screen == Screen::Call => return Some(Action::SendFile),
            KeyCode::F(4) if app.screen == Screen::Call => return Some(Action::CancelFile),
            KeyCode::F(5) => return Some(Action::Update),
            KeyCode::F(6) => return Some(Action::ClipboardPaste),
            _ => {}
        }
    }

    if app.edit.is_some() {
        return match k.code {
            KeyCode::Backspace => Some(Action::Backspace),
            KeyCode::Enter | KeyCode::Esc if k.kind != KeyEventKind::Repeat => {
                Some(Action::Confirm)
            }
            KeyCode::Char(c) if plain(k.modifiers) => Some(Action::Insert(typed(c, k.modifiers))),
            _ => None,
        };
    }

    if app.dev_pick.is_some() {
        return match k.code {
            KeyCode::Up => Some(Action::Up),
            KeyCode::Down => Some(Action::Down),
            KeyCode::Enter => Some(Action::Confirm),
            KeyCode::Esc => Some(Action::Cancel),
            _ => None,
        };
    }

    if app.screen == Screen::Connect
        || (app.focus == Focus::Input && matches!(app.screen, Screen::Call | Screen::Settings))
    {
        let repeating = k.kind == KeyEventKind::Repeat;
        return match k.code {
            KeyCode::Backspace => Some(Action::Backspace),
            KeyCode::Left => Some(Action::Left),
            KeyCode::Right => Some(Action::Right),
            KeyCode::Home => Some(Action::Home),
            KeyCode::End => Some(Action::End),
            KeyCode::Delete => Some(Action::Delete),
            KeyCode::Enter if !repeating => Some(Action::Confirm),
            KeyCode::Esc if !repeating => Some(Action::Cancel),
            KeyCode::Char(c) if plain(k.modifiers) => Some(Action::Insert(typed(c, k.modifiers))),
            _ => None,
        };
    }

    // PTT слушает и повтор, пока клавиша удерживается.
    if k.code == KeyCode::Char(' ') && app.screen == Screen::Call && !vox && plain(k.modifiers) {
        return Some(Action::Ptt(true));
    }
    if k.kind == KeyEventKind::Repeat {
        // повтор разрешён только тексту и навигации настроек, как и раньше
        let navigation = matches!(
            k.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        if !navigation {
            return None;
        }
    }
    if app.screen == Screen::Call && k.modifiers == KeyModifiers::CONTROL {
        match k.code {
            KeyCode::Up => return Some(Action::GainPeer(5)),
            KeyCode::Down => return Some(Action::GainPeer(-5)),
            _ => {}
        }
    }
    if !plain(k.modifiers) {
        return None;
    }

    if let KeyCode::Char(c) = k.code {
        if latin(c) == '?' {
            return Some(Action::Help);
        }
    }

    screen_command(k, app)
}

fn screen_command(k: &KeyEvent, app: &App) -> Option<Action> {
    match app.screen {
        Screen::Home => match k.code {
            KeyCode::Up => Some(Action::Up),
            KeyCode::Down => Some(Action::Down),
            KeyCode::Enter => Some(Action::Confirm),
            KeyCode::Char(c) => Some(match latin(c).to_ascii_lowercase() {
                'k' => Action::Up,
                'j' => Action::Down,
                'h' => Action::Host,
                'c' => Action::Connect,
                's' => Action::OpenSettings,
                'q' => Action::Quit,
                _ => return None,
            }),
            _ => None,
        },
        Screen::Connect => match k.code {
            KeyCode::Enter => Some(Action::Confirm),
            KeyCode::Esc => Some(Action::Cancel),
            _ => None,
        },
        Screen::HostWait => match k.code {
            KeyCode::Esc => Some(Action::Cancel),
            KeyCode::Up => Some(Action::Up),
            KeyCode::Down => Some(Action::Down),
            KeyCode::Enter => Some(Action::Confirm),
            KeyCode::Char(c) => match latin(c).to_ascii_lowercase() {
                'q' => Some(Action::Cancel),
                's' => Some(Action::OpenSettings),
                'y' => Some(Action::Confirm),
                _ => None,
            },
            _ => None,
        },
        Screen::Incoming => match k.code {
            KeyCode::Enter => Some(Action::Accept),
            KeyCode::Esc => Some(Action::Decline),
            KeyCode::Char(c) if latin(c).to_ascii_lowercase() == 's' => Some(Action::OpenSettings),
            _ => None,
        },
        Screen::Call => match k.code {
            KeyCode::Up => Some(Action::Up),
            KeyCode::Down => Some(Action::Down),
            KeyCode::Home => Some(Action::Home),
            KeyCode::End => Some(Action::End),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::Tab => Some(Action::NextPane),
            KeyCode::BackTab => Some(Action::NextLink(true)),
            KeyCode::Enter if app.focus == Focus::Pane(Pane::Side) => {
                Some(Action::ShowProfile(app.call_sel))
            }
            KeyCode::Enter => Some(Action::Confirm),
            KeyCode::Esc => Some(Action::Cancel),
            KeyCode::Char(c) => {
                let code = typed(latin(c), k.modifiers);
                let lower = code.to_ascii_lowercase();
                Some(match code {
                    'O' => Action::DeviceCycle { input: false },
                    'I' => Action::DeviceCycle { input: true },
                    _ => match lower {
                        'y' if app.focus == Focus::Pane(Pane::Main) => Action::FileDecision(true),
                        'n' if app.focus == Focus::Pane(Pane::Main) => Action::FileDecision(false),
                        'm' => Action::ToggleMute,
                        'd' => Action::ToggleDeaf,
                        's' => Action::OpenSettings,
                        'i' | 't' => Action::FocusInput,
                        'l' => Action::NextLink(false),
                        'a' => Action::ToggleAutoGain,
                        'u' => Action::HangUp,
                        '[' => Action::GainMic(-5),
                        ']' => Action::GainMic(5),
                        // RU-клавиша «м» тоже глушит микрофон: привычная подпись M.
                        _ if c == 'м' || c == 'М' => Action::ToggleMute,
                        _ => return None,
                    },
                })
            }
            _ => None,
        },
        Screen::Settings => match k.code {
            KeyCode::Up => Some(Action::Up),
            KeyCode::Down => Some(Action::Down),
            KeyCode::Left => Some(Action::Left),
            KeyCode::Right => Some(Action::Right),
            KeyCode::Enter | KeyCode::Char(' ') => Some(Action::Confirm),
            KeyCode::Esc => Some(Action::Cancel),
            KeyCode::Char(c) if latin(c).to_ascii_lowercase() == 'l' => Some(Action::ToggleEcho),
            _ => None,
        },
    }
}

fn plain(modifiers: KeyModifiers) -> bool {
    !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

fn typed(c: char, modifiers: KeyModifiers) -> char {
    if modifiers.contains(KeyModifiers::SHIFT) && c.is_lowercase() {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c
    }
}

/// Русская буква → латинская на той же физической клавише.
fn latin(c: char) -> char {
    let lower = c.to_lowercase().next().unwrap_or(c);
    let mapped = match lower {
        'й' => 'q',
        'ц' => 'w',
        'у' => 'e',
        'к' => 'r',
        'е' => 't',
        'н' => 'y',
        'г' => 'u',
        'ш' => 'i',
        'щ' => 'o',
        'з' => 'p',
        'х' => '[',
        'ъ' => ']',
        'ф' => 'a',
        'ы' => 's',
        'в' => 'd',
        'а' => 'f',
        'п' => 'g',
        'р' => 'h',
        'о' => 'j',
        'л' => 'k',
        'д' => 'l',
        'ж' => ';',
        'э' => '\'',
        'я' => 'z',
        'ч' => 'x',
        'с' => 'c',
        'м' => 'v',
        'и' => 'b',
        'т' => 'n',
        'ь' => 'm',
        'б' => ',',
        'ю' => '.',
        _ => lower,
    };
    if c.is_uppercase() {
        mapped.to_ascii_uppercase()
    } else {
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ROW_ECHO, ROW_NAME};
    use crate::config::Config;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    #[test]
    fn clipboard_keys_and_media_controls_do_not_become_ptt_or_chat_input() {
        let mut app = App::for_test(Config::default());
        app.screen = Screen::Call;
        app.focus = Focus::Input;
        for key in [
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Insert, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
        ] {
            assert_eq!(action_for(&key, &app), Some(Action::ClipboardPaste));
        }
        app.overlay = Some(crate::action::Overlay::Media);
        assert_eq!(
            action_for(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &app),
            Some(Action::MediaPause)
        );
        assert_eq!(
            action_for(&KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE), &app),
            Some(Action::OpenExternal)
        );
        assert_eq!(
            action_for(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &app),
            Some(Action::PageDown)
        );
        app.engine.params.set_vox_mode(false);
        assert_eq!(
            action_for(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &app),
            Some(Action::Ptt(true))
        );
    }

    fn app() -> App {
        App::for_test(Config::default())
    }

    fn call() -> App {
        let mut app = app();
        app.screen = Screen::Call;
        app
    }

    #[test]
    fn repeat_commands_do_not_toggle_audio_and_shift_devices_work() {
        let app = call();
        for c in ['m', 'd', 'a', 'u', 's', '?'] {
            let repeat =
                KeyEvent::new_with_kind(KeyCode::Char(c), KeyModifiers::NONE, KeyEventKind::Repeat);
            assert_eq!(action_for(&repeat, &app), None, "repeated {c}");
        }
        assert_eq!(
            action_for(
                &KeyEvent::new(KeyCode::Char('i'), KeyModifiers::SHIFT),
                &app
            ),
            Some(Action::DeviceCycle { input: true })
        );
        assert_eq!(
            action_for(
                &KeyEvent::new(KeyCode::Char('щ'), KeyModifiers::SHIFT),
                &app
            ),
            Some(Action::DeviceCycle { input: false })
        );
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn release(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Release)
    }

    #[test]
    fn held_enter_does_not_repeat_confirm_or_cancel() {
        let mut app = call();
        app.focus = Focus::Input;
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(
            action_for(&key(KeyCode::Enter), &app),
            Some(Action::Confirm)
        );
        assert_eq!(action_for(&repeat, &app), None);
        app.screen = Screen::Connect;
        assert_eq!(action_for(&repeat, &app), None);
        app.overlay = Some(crate::action::Overlay::HangUp);
        assert_eq!(action_for(&repeat, &app), None);
    }

    #[test]
    fn ru_layout_maps_to_physical_keys() {
        let mut app = call();
        app.focus = Focus::Pane(Pane::Main);
        assert_eq!(
            action_for(&key(KeyCode::Char('м')), &app),
            Some(Action::ToggleMute)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('ь')), &app),
            Some(Action::ToggleMute)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('в')), &app),
            Some(Action::ToggleDeaf)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('ы')), &app),
            Some(Action::OpenSettings)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('ш')), &app),
            Some(Action::FocusInput)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('ф')), &app),
            Some(Action::ToggleAutoGain)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('г')), &app),
            Some(Action::HangUp)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('х')), &app),
            Some(Action::GainMic(-5))
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('ъ')), &app),
            Some(Action::GainMic(5))
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('Щ')), &app),
            Some(Action::DeviceCycle { input: false })
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('Ш')), &app),
            Some(Action::DeviceCycle { input: true })
        );
        assert_eq!(action_for(&key(KeyCode::Char('щ')), &app), None);
    }

    #[test]
    fn question_mark_opens_help() {
        let app = call();
        assert_eq!(
            action_for(&key(KeyCode::Char('?')), &app),
            Some(Action::Help)
        );
    }

    #[test]
    fn ptt_holds_in_ptt_mode_and_ignores_vox() {
        let mut app = call();
        app.focus = Focus::Pane(Pane::Main);
        app.engine.params.set_vox_mode(false);
        let press = key(KeyCode::Char(' '));
        assert_eq!(action_for(&press, &app), Some(Action::Ptt(true)));
        assert_eq!(
            action_for(&release(KeyCode::Char(' ')), &app),
            Some(Action::Ptt(false))
        );

        app.engine.params.set_vox_mode(true);
        assert_eq!(action_for(&press, &app), None);
        assert_eq!(
            action_for(&release(KeyCode::Char(' ')), &app),
            Some(Action::Ptt(false))
        );
    }

    #[test]
    fn shift_uppercases_typed_text_and_esc_blurs_input() {
        let mut app = call();
        app.focus = Focus::Input;
        assert_eq!(
            action_for(&key_mod(KeyCode::Char('a'), KeyModifiers::SHIFT), &app),
            Some(Action::Insert('A'))
        );
        assert_eq!(
            action_for(&key_mod(KeyCode::Char('ж'), KeyModifiers::SHIFT), &app),
            Some(Action::Insert('Ж'))
        );
        assert_eq!(
            action_for(&key(KeyCode::Char(':')), &app),
            Some(Action::Insert(':'))
        );
        assert_eq!(action_for(&key(KeyCode::Esc), &app), Some(Action::Cancel));
        app.on_action(Action::Cancel);
        assert_eq!(app.focus, Focus::Pane(Pane::Main));
    }

    #[test]
    fn call_navigation_depends_on_focus() {
        let mut app = call();
        app.focus = Focus::Pane(Pane::Side);
        assert_eq!(action_for(&key(KeyCode::Up), &app), Some(Action::Up));
        assert_eq!(action_for(&key(KeyCode::Down), &app), Some(Action::Down));
        app.focus = Focus::Pane(Pane::Main);
        assert_eq!(action_for(&key(KeyCode::Up), &app), Some(Action::Up));
        assert_eq!(action_for(&key(KeyCode::Down), &app), Some(Action::Down));
        assert_eq!(
            action_for(&key(KeyCode::PageUp), &app),
            Some(Action::PageUp)
        );
        assert_eq!(
            action_for(&key(KeyCode::PageDown), &app),
            Some(Action::PageDown)
        );
        assert_eq!(action_for(&key(KeyCode::Tab), &app), Some(Action::NextPane));
        assert_eq!(action_for(&key(KeyCode::Esc), &app), Some(Action::Cancel));
    }

    #[test]
    fn other_screens_map_their_keys() {
        let mut app = app();
        assert_eq!(
            action_for(&key(KeyCode::Char('h')), &app),
            Some(Action::Host)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('с')), &app),
            Some(Action::Connect)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('ы')), &app),
            Some(Action::OpenSettings)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('й')), &app),
            Some(Action::Quit)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('о')), &app),
            Some(Action::Down)
        );
        assert_eq!(action_for(&key(KeyCode::Char('л')), &app), Some(Action::Up));

        app.screen = Screen::Connect;
        assert_eq!(
            action_for(&key(KeyCode::Enter), &app),
            Some(Action::Confirm)
        );
        assert_eq!(action_for(&key(KeyCode::Esc), &app), Some(Action::Cancel));

        app.screen = Screen::HostWait;
        assert_eq!(
            action_for(&key(KeyCode::Char('q')), &app),
            Some(Action::Cancel)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('s')), &app),
            Some(Action::OpenSettings)
        );

        app.screen = Screen::Incoming;
        assert_eq!(action_for(&key(KeyCode::Enter), &app), Some(Action::Accept));
        assert_eq!(action_for(&key(KeyCode::Esc), &app), Some(Action::Decline));
        assert_eq!(
            action_for(&key(KeyCode::Char('s')), &app),
            Some(Action::OpenSettings)
        );
    }

    #[test]
    fn settings_and_edit_keys() {
        let mut app = app();
        app.screen = Screen::Settings;
        app.settings_sel = ROW_ECHO;
        assert_eq!(
            action_for(&key(KeyCode::Char('l')), &app),
            Some(Action::ToggleEcho)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('д')), &app),
            Some(Action::ToggleEcho)
        );
        assert_eq!(action_for(&key(KeyCode::Left), &app), Some(Action::Left));
        assert_eq!(action_for(&key(KeyCode::Right), &app), Some(Action::Right));
        assert_eq!(
            action_for(&key(KeyCode::Char(' ')), &app),
            Some(Action::Confirm)
        );
        assert_eq!(action_for(&key(KeyCode::Tab), &app), None);
        assert_eq!(action_for(&key(KeyCode::Esc), &app), Some(Action::Cancel));

        app.settings_sel = ROW_NAME;
        app.edit = Some((ROW_NAME, String::new()));
        assert_eq!(
            action_for(
                &KeyEvent::new_with_kind(
                    KeyCode::Backspace,
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat
                ),
                &app
            ),
            Some(Action::Backspace)
        );
        assert_eq!(
            action_for(&key_mod(KeyCode::Char('a'), KeyModifiers::SHIFT), &app),
            Some(Action::Insert('A'))
        );
        assert_eq!(
            action_for(&key(KeyCode::Enter), &app),
            Some(Action::Confirm)
        );
        app.edit = None;
        assert_eq!(action_for(&key(KeyCode::Char('m')), &app), None);
    }

    #[test]
    fn overlay_takes_keys() {
        let mut app = call();
        app.overlay = Some(crate::action::Overlay::Help);
        assert_eq!(action_for(&key(KeyCode::Char('m')), &app), None);
        assert_eq!(action_for(&key(KeyCode::Esc), &app), Some(Action::Cancel));
        assert_eq!(
            action_for(&key(KeyCode::Enter), &app),
            Some(Action::Confirm)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('?')), &app),
            Some(Action::Cancel)
        );
        assert_eq!(action_for(&key(KeyCode::F(1)), &app), None);
        assert_eq!(action_for(&key(KeyCode::F(2)), &app), None);
    }
}
