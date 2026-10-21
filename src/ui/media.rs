use super::{Ui, accent, dim, strong};
use crate::{
    action::{Action, Pane},
    app::App,
    media::Content,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

pub fn area(body: Rect) -> Rect {
    super::centered_rect(
        body,
        body.width.saturating_sub(4).max(10),
        body.height.saturating_sub(2).max(6),
    )
}
pub fn render(app: &App, f: &mut Frame, ui: &mut Ui, body: Rect) {
    let Some(viewer) = &app.media else {
        return;
    };
    let area = area(body);
    f.render_widget(Clear, area);
    let filename = viewer
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let title: String = filename.chars().filter(|c| !c.is_control()).collect();
    let block = Block::bordered()
        .title(format!(
            " {} ",
            super::cut(&title, area.width.saturating_sub(5) as usize)
        ))
        .title_bottom(" Esc закрыть · O открыть внешне ")
        .border_style(accent());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [info, image, buttons] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    ui.push_pane(image, Pane::Main);
    let mut actions = vec![("[ O внешне ]", Action::OpenExternal)];
    match &viewer.content {
        Some(Content::Image(frames)) => {
            let frame = &frames[viewer.frame];
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{}×{}", frame.image.width(), frame.image.height()),
                        dim(),
                    ),
                    Span::styled(
                        if frames.len() > 1 {
                            format!(
                                " · GIF · кадр {}/{} · {}",
                                viewer.frame + 1,
                                frames.len(),
                                if viewer.paused { "пауза" } else { "▶" }
                            )
                        } else {
                            String::new()
                        },
                        dim(),
                    ),
                ])),
                info,
            );
            let area = if frames.len() > 1 {
                ui.thumbnails.animation_area(image)
            } else {
                image
            };
            ui.thumbnails.render_image(&frame.image, f, area);
            if frames.len() > 1 {
                actions.push((
                    if viewer.paused {
                        "[ P ▶ ]"
                    } else {
                        "[ P пауза ]"
                    },
                    Action::MediaPause,
                ));
            }
        }
        Some(Content::Text(text)) => {
            f.render_widget(
                Paragraph::new("↑↓ / PgUp / PgDn · Home / End — прокрутка").style(dim()),
                info,
            );
            let mut layout = viewer.wrapped.borrow_mut();
            if layout
                .as_ref()
                .is_none_or(|(width, _)| *width != image.width)
            {
                *layout = Some((image.width, crate::media::wrap_text(text, image.width)));
            }
            let lines = &layout.as_ref().unwrap().1;
            viewer.text_rows.set(lines.len());
            let rows: Vec<Line> = lines
                .iter()
                .skip(viewer.scroll)
                .take(image.height as usize)
                .map(|s| Line::from(s.as_str()))
                .collect();
            f.render_widget(Paragraph::new(rows).style(strong()), image);
        }
        None => {
            f.render_widget(
                Paragraph::new(
                    viewer
                        .error
                        .as_deref()
                        .unwrap_or("Загружаем файл… голос продолжает работать"),
                )
                .style(dim())
                .wrap(Wrap { trim: false }),
                image,
            );
        }
    }
    actions.push(("[ Закрыть ]", Action::Cancel));
    let mut x = buttons.x;
    for (text, action) in actions {
        let width = (text.chars().count() as u16).min(buttons.right().saturating_sub(x));
        if width > 0 {
            let rect = Rect::new(x, buttons.y, width, buttons.height);
            f.render_widget(Paragraph::new(text).style(accent()), rect);
            ui.push(rect, action);
            x = x.saturating_add(width + 1);
        }
    }
}
