//! Chat layout and explicit user-initiated desktop opening (never a shell).
use crate::app::ChatLine;
use crate::transfer::TransferStatus;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    Url(String),
    File(PathBuf),
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub area: Rect,
    pub index: usize,
}
pub struct Row {
    pub line: Line<'static>,
    pub hits: Vec<(u16, u16, usize)>,
}
pub struct Layout {
    pub rows: Vec<Row>,
    pub targets: Vec<OpenTarget>,
    pub target_rows: Vec<usize>,
}

pub struct LayoutCache {
    pub messages: usize,
    pub width: u16,
    pub selected: Option<usize>,
    pub layout: Layout,
}

pub fn urls(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find("http") {
        let start = cursor + offset;
        let rest = &lower[start..];
        if !rest.starts_with("https://") && !rest.starts_with("http://") {
            cursor = start + 4;
            continue;
        }
        let mut end = text[start..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || c.is_control() || "<>\"'`\\".contains(*c))
            .map_or(text.len(), |(i, _)| start + i);
        loop {
            let s = &text[start..end];
            let Some(c) = s.chars().last() else { break };
            let opening = match c {
                ')' => Some('('),
                ']' => Some('['),
                '}' => Some('{'),
                _ => None,
            };
            if ".,;!".contains(c)
                || opening.is_some_and(|o| s.matches(c).count() > s.matches(o).count())
            {
                end -= c.len_utf8();
            } else {
                break;
            }
        }
        if valid_url(&text[start..end]) {
            found.push((start, end));
        }
        cursor = (start + 4).max(end);
    }
    found
}
pub fn valid_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty()
        && authority != "."
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "\\<>\"'`".contains(c))
}

pub fn layout(chat: &[ChatLine], width: u16, selected: Option<usize>) -> Layout {
    let mut result = Layout {
        rows: Vec::new(),
        targets: Vec::new(),
        target_rows: Vec::new(),
    };
    if width == 0 {
        return result;
    }
    for message in chat {
        let who = if message.mine {
            format!("{} (вы): ", message.name)
        } else {
            format!("{}: ", message.name)
        };
        let mut parts = vec![(
            who,
            Style::new().fg(if message.mine {
                Color::Cyan
            } else {
                Color::Green
            }),
            None,
        )];
        if let Some(file) = &message.attachment {
            let target = if file.status == TransferStatus::Complete {
                file.path.clone().map(OpenTarget::File)
            } else {
                None
            };
            let index = target.map(|t| {
                let i = result.targets.len();
                result.targets.push(t);
                i
            });
            parts.push((file.label(), Style::new(), index));
        } else {
            let mut cursor = 0;
            for (start, end) in urls(&message.text) {
                parts.push((message.text[cursor..start].to_owned(), Style::new(), None));
                let index = result.targets.len();
                result
                    .targets
                    .push(OpenTarget::Url(message.text[start..end].to_owned()));
                parts.push((
                    message.text[start..end].to_owned(),
                    Style::new(),
                    Some(index),
                ));
                cursor = end;
            }
            parts.push((message.text[cursor..].to_owned(), Style::new(), None));
            if message.mine {
                parts.push((
                    match message.status {
                        0 => " …",
                        1 => " ✓",
                        2 => " ✗не доставлено",
                        _ => "",
                    }
                    .to_owned(),
                    Style::new().fg(Color::DarkGray),
                    None,
                ));
            }
        }
        let mut row = Row {
            line: Line::default(),
            hits: Vec::new(),
        };
        let mut column = 0u16;
        for (text, style, target) in parts {
            let style = if let Some(i) = target {
                Style::new().fg(Color::Cyan).add_modifier(
                    Modifier::UNDERLINED
                        | if Some(i) == selected {
                            Modifier::REVERSED
                        } else {
                            Modifier::empty()
                        },
                )
            } else {
                style
            };
            let mut segment = String::new();
            let mut segment_start = column;
            for c in text.chars() {
                let c = if c.is_control() && c != '\n' { ' ' } else { c };
                let cells = c.width().unwrap_or(0) as u16;
                if c == '\n' || column + cells > width {
                    push_segment(&mut row, &mut segment, style, target, segment_start, column);
                    result.rows.push(row);
                    row = Row {
                        line: Line::default(),
                        hits: Vec::new(),
                    };
                    column = 0;
                    segment_start = 0;
                    if c == '\n' {
                        continue;
                    }
                }
                if cells > width {
                    continue;
                }
                if let Some(index) = target {
                    while result.target_rows.len() <= index {
                        result.target_rows.push(result.rows.len());
                    }
                }
                segment.push(c);
                column += cells;
            }
            push_segment(&mut row, &mut segment, style, target, segment_start, column);
        }
        result.rows.push(row);
    }
    result
}
fn push_segment(
    row: &mut Row,
    text: &mut String,
    style: Style,
    target: Option<usize>,
    start: u16,
    end: u16,
) {
    if !text.is_empty() {
        row.line
            .spans
            .push(Span::styled(std::mem::take(text), style));
    }
    if let Some(index) = target
        && end > start
    {
        row.hits.push((start, end - start, index));
    }
}

pub fn open(target: OpenTarget) -> Result<(), String> {
    let argument = match target {
        OpenTarget::Url(url) if valid_url(&url) => url,
        OpenTarget::Url(_) => return Err("поддерживаются только ссылки http:// и https://".into()),
        OpenTarget::File(path) => path
            .canonicalize()
            .map_err(|e| format!("открыть файл: {e}"))?
            .to_string_lossy()
            .into_owned(),
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut cmd = std::process::Command::new("rundll32.exe");
        cmd.arg("url.dll,FileProtocolHandler");
        cmd
    };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    let result = command
        .arg(argument)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("запуск приложения: {e}"))?;
    if result.success() {
        Ok(())
    } else {
        Err(format!("приложение открытия завершилось с {result}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_unicode_queries_and_balanced_punctuation_without_executable_schemes() {
        let text = "Пример (https://пример.рф/a(b)?q=Я&x=2), HTTPS://example.org/x. file:///etc/passwd javascript:alert(1) https://";
        let actual: Vec<_> = urls(text).into_iter().map(|(a, b)| &text[a..b]).collect();
        assert_eq!(
            actual,
            ["https://пример.рф/a(b)?q=Я&x=2", "HTTPS://example.org/x"]
        );
        for bad in [
            "file:///tmp/a",
            "javascript:alert(1)",
            "https://",
            "https://x\nfile:///x",
            "https://x\\evil",
        ] {
            assert!(!valid_url(bad));
        }
    }
    #[test]
    fn wrapped_unicode_links_keep_exact_mouse_targets() {
        let chat = vec![ChatLine {
            mine: false,
            name: "界界".into(),
            text: "см. https://пример.рф/длинная-ссылка и https://other.org".into(),
            status: 3,
            attachment: None,
        }];
        let layout = layout(&chat, 20, Some(0));
        assert_eq!(layout.targets.len(), 2);
        assert!(layout.rows.len() > 2);
        let hits: Vec<_> = layout.rows.iter().flat_map(|r| r.hits.iter()).collect();
        assert!(hits.iter().filter(|h| h.2 == 0).count() > 1);
        assert!(hits.iter().all(|h| h.0 + h.1 <= 20));
        assert!(layout.target_rows[1] > layout.target_rows[0]);
    }
}
