// Учёт кликабельных прямоугольников: последний добавленный — верхний.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::action::{Action, Pane};

#[derive(Default)]
pub struct Ui {
    hits: Vec<(Rect, Action)>,
    panes: Vec<(Rect, Pane)>,
}

impl Ui {
    pub fn clear(&mut self) {
        self.hits.clear();
        self.panes.clear();
    }

    pub fn push(&mut self, rect: Rect, action: Action) {
        self.hits.push((rect, action));
    }

    pub fn push_pane(&mut self, rect: Rect, pane: Pane) {
        self.panes.push((rect, pane));
    }

    pub fn hit(&self, col: u16, row: u16) -> Option<Action> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| inside(*rect, col, row))
            .map(|(_, action)| action.clone())
    }

    pub fn mouse(&self, m: MouseEvent) -> Option<Action> {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.hit(m.column, m.row),
            MouseEventKind::ScrollUp => self
                .pane_at(m.column, m.row)
                .map(|pane| Action::Scroll(pane, -3)),
            MouseEventKind::ScrollDown => self
                .pane_at(m.column, m.row)
                .map(|pane| Action::Scroll(pane, 3)),
            _ => None,
        }
    }

    fn pane_at(&self, col: u16, row: u16) -> Option<Pane> {
        self.panes
            .iter()
            .rev()
            .find(|(rect, _)| inside(*rect, col, row))
            .map(|(_, pane)| *pane)
    }
}

fn inside(rect: Rect, col: u16, row: u16) -> bool {
    let right = rect.x.saturating_add(rect.width);
    let bottom = rect.y.saturating_add(rect.height);
    col >= rect.x && col < right && row >= rect.y && row < bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, Pane};
    use crossterm::event::KeyModifiers;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn left_click_returns_action_inside_rect() {
        let mut ui = Ui::default();
        ui.push(Rect::new(2, 3, 5, 2), Action::Confirm);
        assert_eq!(
            ui.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 4)),
            Some(Action::Confirm)
        );
        assert_eq!(
            ui.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 7, 4)),
            None
        );
    }

    #[test]
    fn wheel_scrolls_pane_under_cursor() {
        let mut ui = Ui::default();
        ui.push_pane(Rect::new(0, 0, 10, 10), Pane::Main);
        assert_eq!(
            ui.mouse(mouse(MouseEventKind::ScrollUp, 4, 4)),
            Some(Action::Scroll(Pane::Main, -3))
        );
        assert_eq!(
            ui.mouse(mouse(MouseEventKind::ScrollDown, 4, 4)),
            Some(Action::Scroll(Pane::Main, 3))
        );
        assert_eq!(ui.mouse(mouse(MouseEventKind::ScrollDown, 40, 40)), None);
    }

    #[test]
    fn topmost_rect_wins() {
        let mut ui = Ui::default();
        ui.push(Rect::new(0, 0, 10, 10), Action::Up);
        ui.push(Rect::new(2, 2, 4, 4), Action::Down);
        assert_eq!(ui.hit(3, 3), Some(Action::Down));
        assert_eq!(ui.hit(9, 9), Some(Action::Up));
    }
}
