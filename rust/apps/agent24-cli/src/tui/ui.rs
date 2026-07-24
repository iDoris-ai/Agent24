//! ratatui rendering — pure view over [`App`] state (no state mutation here).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use agent24_protocol::RunStatus;

use super::app::{App, Focus};

pub fn draw(f: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(f.area());

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(32),
            Constraint::Percentage(40),
            Constraint::Percentage(28),
        ])
        .split(root[0]);

    draw_runs(f, app, cols[0]);
    draw_events(f, app, cols[1]);
    draw_approvals(f, app, cols[2]);
    draw_statusline(f, app, root[1]);

    if app.modal().is_some() {
        draw_modal(f, app);
    }
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn status_span(status: RunStatus) -> Span<'static> {
    let (label, color) = match status {
        RunStatus::Queued => ("queued", Color::Gray),
        RunStatus::Running => ("running", Color::Yellow),
        RunStatus::AwaitingApproval => ("await-approval", Color::Magenta),
        RunStatus::Completed => ("completed", Color::Green),
        RunStatus::Failed => ("failed", Color::Red),
        RunStatus::Cancelled => ("cancelled", Color::DarkGray),
    };
    Span::styled(format!("{label:<14}"), Style::default().fg(color))
}

fn draw_runs(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus() == Focus::Runs;
    let items: Vec<ListItem> = app
        .runs()
        .iter()
        .enumerate()
        .map(|(i, run)| {
            let marker = if i == app.run_cursor() && focused {
                "▶ "
            } else {
                "  "
            };
            let prompt: String = run.input.prompt.chars().take(28).collect();
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                status_span(run.status),
                Span::raw(" "),
                Span::styled(prompt, Style::default().fg(Color::White)),
            ]))
        })
        .collect();
    let list = List::new(if items.is_empty() {
        vec![ListItem::new(
            "(no runs — POST /api/v1/runs or run a schedule)",
        )]
    } else {
        items
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(focus_style(focused))
            .title(" Runs "),
    );
    f.render_widget(list, area);
}

fn draw_events(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_run() {
        Some(run) => format!(" Events · {} ", run.id),
        None => " Events ".to_owned(),
    };
    let lines: Vec<Line> = app
        .selected_run_events()
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .rev()
        .map(|l| Line::from(l.clone()))
        .collect();
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(title),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(para, area);
}

fn draw_approvals(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus() == Focus::Approvals;
    let items: Vec<ListItem> = app
        .approvals()
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let marker = if i == app.approval_cursor() && focused {
                "▶ "
            } else {
                "  "
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(
                    format!("[{}] ", a.kind),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(a.summary.chars().take(22).collect::<String>()),
            ]))
        })
        .collect();
    let list = List::new(if items.is_empty() {
        vec![ListItem::new("(no pending approvals)")]
    } else {
        items
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(focus_style(focused))
            .title(format!(" Approvals ({}) ", app.approvals().len())),
    );
    f.render_widget(list, area);
}

fn draw_statusline(f: &mut Frame, app: &App, area: Rect) {
    let hint = if app.modal().is_some() {
        "↑/↓ choose · Enter confirm · Esc cancel (no decision)"
    } else {
        "Tab switch · ↑/↓ move · Enter approve-queue → decide · c cancel run · q quit"
    };
    let recon = if app.needs_reconcile {
        Span::styled("  ⟳ reconciling", Style::default().fg(Color::Yellow))
    } else {
        Span::raw("")
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(hint, Style::default().fg(Color::DarkGray)),
            recon,
        ])),
        area,
    );
}

fn draw_modal(f: &mut Frame, app: &App) {
    let Some(modal) = app.modal() else { return };
    let area = centered_rect(60, 50, f.area());
    f.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(Span::styled(
            modal.approval.summary.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(
                "kind: {} · run {}",
                modal.approval.kind, modal.approval.run_id
            ),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    if let Some(reason) = &modal.reason {
        lines.push(Line::from("Deny reason (Enter to submit, Esc to go back):"));
        lines.push(Line::from(Span::styled(
            format!("> {reason}▏"),
            Style::default().fg(Color::Yellow),
        )));
    } else {
        for (i, decision) in modal.approval.available_decisions.iter().enumerate() {
            let selected = i == modal.cursor;
            let style = if selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(Span::styled(
                format!("{} {}", if selected { "▶" } else { " " }, decision),
                style,
            )));
        }
    }

    let modal_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(" Approval required "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(modal_widget, area);
}

fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vert[1])[1]
}
