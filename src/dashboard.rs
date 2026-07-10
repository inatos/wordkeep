//! Optional terminal dashboard for the savings telemetry.
//!
//! Built only under the `dashboard` feature and launched as a subcommand:
//!
//! ```sh
//! cargo run --release --features dashboard -- dashboard
//! ```
//!
//! It runs as a *separate process* from the MCP server: once a second it polls
//! `savings.json` through [`crate::stats::read_snapshot`] and charts realtime
//! throughput, per-tool high-water marks, a rolling activity log, and health
//! flags (tools whose returned tokens exceed the distilled estimate). The lean
//! default binary contains none of this - `ratatui` is an optional dependency
//! pulled in only by the `dashboard` feature.

use std::io::{self, Stdout};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

use crate::stats::{self, Snapshot, ToolStat};

/// Per-tool cumulative saved tokens captured when the dashboard starts.
type SessionBaseline = std::collections::BTreeMap<String, u64>;

/// How often the dashboard re-reads `savings.json` and redraws.
const TICK: Duration = Duration::from_secs(1);

/// Entry point for the `dashboard` subcommand. Sets up the alternate screen and
/// raw mode, runs the event/redraw loop, and always restores the terminal on
/// the way out - even if the loop returns an error.
pub fn run() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let baseline = session_baseline(&stats::read_snapshot());
    let res = event_loop(&mut terminal, baseline);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    baseline: SessionBaseline,
) -> io::Result<()> {
    loop {
        let snap = stats::read_snapshot();
        terminal.draw(|f| draw(f, &snap, &baseline))?;

        // Block up to one tick for input; on timeout we fall through and redraw
        // with a fresh snapshot, which is what makes the view "realtime".
        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    let ctrl_c =
                        k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
                    if ctrl_c || matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn session_baseline(snap: &Snapshot) -> SessionBaseline {
    snap.tools
        .iter()
        .map(|t| {
            let saved = t.baseline_tokens.saturating_sub(t.returned_tokens);
            (t.name.clone(), saved)
        })
        .collect()
}

fn session_saved(name: &str, baseline: &SessionBaseline, current_saved: u64) -> u64 {
    current_saved.saturating_sub(*baseline.get(name).unwrap_or(&0))
}

fn draw(f: &mut Frame, snap: &Snapshot, baseline: &SessionBaseline) {
    let rows = Layout::vertical([
        Constraint::Length(6),  // overview header (wraps on narrow terminals)
        Constraint::Min(6),     // per-tool table
        Constraint::Length(12), // activity log + health
    ])
    .split(f.area());

    draw_header(f, rows[0], snap, baseline);
    draw_tools(f, rows[1], snap, baseline);

    let bottom =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(rows[2]);
    draw_activity(f, bottom[0], snap);
    draw_health(f, bottom[1], snap);
}

struct Totals {
    calls: u64,
    baseline: u64,
    returned: u64,
    saved: u64,
    session_saved: u64,
    pct: u64,
}

fn totals(snap: &Snapshot, baseline: &SessionBaseline) -> Totals {
    let calls = snap.tools.iter().map(|t| t.calls).sum();
    let baseline_tok: u64 = snap.tools.iter().map(|t| t.baseline_tokens).sum();
    let returned: u64 = snap.tools.iter().map(|t| t.returned_tokens).sum();
    let saved = baseline_tok.saturating_sub(returned);
    let session_saved: u64 = snap
        .tools
        .iter()
        .map(|t| {
            let cur = t.baseline_tokens.saturating_sub(t.returned_tokens);
            session_saved(&t.name, baseline, cur)
        })
        .sum();
    Totals {
        calls,
        baseline: baseline_tok,
        returned,
        saved,
        session_saved,
        pct: stats::pct(saved, baseline_tok),
    }
}

fn draw_header(f: &mut Frame, area: Rect, snap: &Snapshot, baseline: &SessionBaseline) {
    let t = totals(snap, baseline);
    let title = Line::from(vec![
        Span::styled("wordkeep ", bold(Color::Cyan)),
        Span::styled("dashboard", bold(Color::White)),
        Span::raw("    "),
        Span::styled(
            format!("tracking since {}", stats::elapsed(snap.since, snap.now)),
            dim(),
        ),
    ]);
    let figures = Line::from(vec![
        Span::raw("calls "),
        Span::styled(stats::commafy(t.calls), bold(Color::White)),
        Span::raw("    distilled "),
        Span::styled(stats::commafy(t.baseline), Style::new().fg(Color::Yellow)),
        Span::raw("    returned "),
        Span::styled(stats::commafy(t.returned), Style::new().fg(Color::Yellow)),
        Span::raw("    saved "),
        Span::styled(stats::commafy(t.saved), bold(Color::Green)),
        Span::raw("    session "),
        Span::styled(stats::commafy(t.session_saved), bold(Color::Cyan)),
        Span::raw("    "),
        Span::styled(format!("{}% reduction", t.pct), bold(Color::Green)),
    ]);
    let hint = Line::from(Span::styled(
        "q / Esc to quit  ·  refreshes every 1s",
        dim(),
    ));
    let p = Paragraph::new(vec![title, figures, hint])
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" overview "));
    f.render_widget(p, area);
}

fn draw_tools(f: &mut Frame, area: Rect, snap: &Snapshot, baseline: &SessionBaseline) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" tools (by saved) ");
    if snap.tools.is_empty() {
        let p = Paragraph::new(
            "waiting for tool calls… run some wordkeep tools and watch them appear.",
        )
        .style(dim())
        .block(block);
        f.render_widget(p, area);
        return;
    }

    let mut ordered: Vec<&ToolStat> = snap.tools.iter().collect();
    ordered.sort_by(|a, b| {
        let sa = a.baseline_tokens.saturating_sub(a.returned_tokens);
        let sb = b.baseline_tokens.saturating_sub(b.returned_tokens);
        sb.cmp(&sa).then_with(|| a.name.cmp(&b.name))
    });

    let rows: Vec<Row> = ordered
        .iter()
        .map(|t| {
            let saved = t.baseline_tokens.saturating_sub(t.returned_tokens);
            let sess = session_saved(&t.name, baseline, saved);
            let p = stats::pct(saved, t.baseline_tokens);
            let inverted = t.baseline_tokens > 0 && t.returned_tokens >= t.baseline_tokens;
            let pct_style = if inverted {
                Style::new().fg(Color::Red)
            } else if p >= 90 {
                Style::new().fg(Color::Green)
            } else if p >= 50 {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new().fg(Color::Red)
            };
            Row::new(vec![
                Cell::from(t.name.clone()).style(Style::new().fg(Color::White)),
                Cell::from(stats::commafy(t.calls)),
                Cell::from(format!("{}", t.avg_ms())),
                Cell::from(format!("{}", t.trunc_count)),
                Cell::from(format!("{}", t.error_count)),
                Cell::from(stats::commafy(t.baseline_tokens)),
                Cell::from(stats::commafy(t.returned_tokens)),
                Cell::from(stats::commafy(saved)).style(Style::new().fg(Color::Green)),
                Cell::from(stats::commafy(sess)).style(Style::new().fg(Color::Cyan)),
                Cell::from(format!("{} {:>3}%", bar(p, 6), p)).style(pct_style),
                Cell::from(stats::commafy(t.peak_saved)),
                Cell::from(ago(snap.now, t.last_ts)).style(dim()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(12),
        Constraint::Length(7),
        Constraint::Length(7),
    ];
    let header = Row::new(vec![
        "tool",
        "calls",
        "avgms",
        "trunc",
        "err",
        "distill",
        "return",
        "saved",
        "session",
        "reduction",
        "peak",
        "last",
    ])
    .style(bold(Color::DarkGray));
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .block(block);
    f.render_widget(table, area);
}

fn draw_activity(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" recent activity ");
    let cap = area.height.saturating_sub(2).max(1) as usize;
    let lines: Vec<Line> = snap
        .events
        .iter()
        .rev()
        .take(cap)
        .map(|e| {
            let _saved = e.baseline.saturating_sub(e.returned);
            Line::from(vec![
                Span::styled(format!("{:>7}", ago(snap.now, e.ts)), dim()),
                Span::raw("  "),
                Span::styled(format!("{:<14}", e.tool), Style::new().fg(Color::Cyan)),
                Span::raw(format!(" {:>4}ms", e.elapsed_ms)),
                Span::raw(format!(
                    "  {:>7} → {:>5}",
                    stats::commafy(e.baseline),
                    stats::commafy(e.returned)
                )),
                Span::styled(
                    format!("  {}", e.outcome),
                    Style::new().fg(outcome_color(&e.outcome)),
                ),
            ])
        })
        .collect();
    let body = if lines.is_empty() {
        vec![Line::from(Span::styled("no calls yet", dim()))]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(body).block(block), area);
}

fn draw_health(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = Block::default().borders(Borders::ALL).title(" health ");
    let mut lines: Vec<Line> = Vec::new();

    // Watermark: the largest single-call saving seen, and which tool did it.
    let peak = snap.tools.iter().map(|t| t.peak_saved).max().unwrap_or(0);
    let peak_tool = snap
        .tools
        .iter()
        .max_by_key(|t| t.peak_saved)
        .map(|t| t.name.as_str())
        .unwrap_or("-");
    lines.push(Line::from(vec![
        Span::styled("peak single-call save  ", dim()),
        Span::styled(stats::commafy(peak), bold(Color::Green)),
        Span::styled(format!(" ({peak_tool})"), dim()),
    ]));

    // The other two watermarks: biggest single distilled read avoided, and the
    // biggest single distilled answer emitted.
    let peak_in = snap
        .tools
        .iter()
        .map(|t| t.peak_baseline)
        .max()
        .unwrap_or(0);
    let peak_out = snap
        .tools
        .iter()
        .map(|t| t.peak_returned)
        .max()
        .unwrap_or(0);
    lines.push(Line::from(vec![
        Span::styled("peak single-call read  ", dim()),
        Span::styled(stats::commafy(peak_in), Style::new().fg(Color::Yellow)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("peak single-call emit  ", dim()),
        Span::styled(stats::commafy(peak_out), Style::new().fg(Color::Yellow)),
    ]));

    // Slowest tool by average latency.
    if let Some(slow) = snap
        .tools
        .iter()
        .filter(|t| t.calls > 0)
        .max_by_key(|t| t.avg_ms())
    {
        lines.push(Line::from(vec![
            Span::styled("slowest (avg)      ", dim()),
            Span::styled(slow.name.clone(), Style::new().fg(Color::White)),
            Span::styled(format!(" ({}ms)", slow.avg_ms()), dim()),
        ]));
    }

    // Peak single-call latency watermark.
    if let Some(spike) = snap
        .tools
        .iter()
        .filter(|t| t.peak_ms > 0)
        .max_by_key(|t| t.peak_ms)
    {
        lines.push(Line::from(vec![
            Span::styled("peak single-call ms  ", dim()),
            Span::styled(spike.name.clone(), Style::new().fg(Color::White)),
            Span::styled(format!(" ({}ms)", stats::commafy(spike.peak_ms)), dim()),
        ]));
    }

    // Highest truncation rate among tools with truncations.
    if let Some(tr) = snap
        .tools
        .iter()
        .filter(|t| t.calls > 0 && t.trunc_count > 0)
        .max_by_key(|t| t.trunc_count * 100 / t.calls)
    {
        let rate = tr.trunc_count * 100 / tr.calls;
        lines.push(Line::from(vec![
            Span::styled("high truncation    ", dim()),
            Span::styled(tr.name.clone(), Style::new().fg(Color::Yellow)),
            Span::styled(format!(" ({rate}% of calls)"), dim()),
        ]));
    }

    // Busiest tool by call count.
    if let Some(busy) = snap.tools.iter().max_by_key(|t| t.calls) {
        lines.push(Line::from(vec![
            Span::styled("busiest                ", dim()),
            Span::styled(busy.name.clone(), Style::new().fg(Color::White)),
            Span::styled(format!(" ({} calls)", stats::commafy(busy.calls)), dim()),
        ]));
    }

    // Net-negative tools: the distilled estimate is smaller than what we
    // returned, so wordkeep "spent" more tokens than reading the raw material -
    // usually a cheap tool wrapped in a fixed JSON-RPC envelope. Worth a look.
    let low_yield: Vec<&str> = snap
        .tools
        .iter()
        .filter(|t| t.low_yield_count > 0)
        .map(|t| t.name.as_str())
        .collect();
    if !low_yield.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("low-yield            ", dim()),
            Span::styled(low_yield.join(", "), Style::new().fg(Color::DarkGray)),
        ]));
    }

    let inverted: Vec<&str> = snap
        .tools
        .iter()
        .filter(|t| t.baseline_tokens > 0 && t.returned_tokens >= t.baseline_tokens)
        .map(|t| t.name.as_str())
        .collect();
    lines.push(Line::from(""));
    if inverted.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("✓ ", Style::new().fg(Color::Green)),
            Span::styled("no net-negative tools", Style::new().fg(Color::Green)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            format!("⚠ net-negative ({}):", inverted.len()),
            bold(Color::Red),
        )));
        lines.push(Line::from(Span::styled(
            inverted.join(", "),
            Style::new().fg(Color::Red),
        )));
    }

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

// --- small formatting helpers -------------------------------------------------

/// A unicode block meter, `width` cells wide, filled to `pct` (rounded).
fn bar(pct: u64, width: usize) -> String {
    let filled = (pct.min(100) as usize * width + 50) / 100;
    (0..width)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect()
}

/// Human "Ns/Nm/Nh/Nd ago", or an em dash when the timestamp is unset.
fn ago(now: u64, ts: u64) -> String {
    if ts == 0 {
        return "-".to_string();
    }
    let s = now.saturating_sub(ts);
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3_600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3_600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

fn outcome_color(outcome: &str) -> Color {
    match outcome {
        "error" => Color::Red,
        "truncated" => Color::Yellow,
        "low_yield" => Color::DarkGray,
        _ => Color::Green,
    }
}

fn bold(fg: Color) -> Style {
    Style::new().fg(fg).add_modifier(Modifier::BOLD)
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::EventLog;

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(bar(0, 10), "░░░░░░░░░░");
        assert_eq!(bar(100, 10), "██████████");
        assert_eq!(bar(50, 10), "█████░░░░░");
        // Over-100 clamps, never overflows the width.
        assert_eq!(bar(250, 8).chars().count(), 8);
    }

    #[test]
    fn ago_buckets_by_magnitude() {
        assert_eq!(ago(100, 0), "-");
        assert_eq!(ago(100, 90), "10s ago");
        assert_eq!(ago(1_000, 100), "15m ago");
        assert_eq!(ago(10_000, 100), "2h ago");
        assert_eq!(ago(200_000, 100), "2d ago");
    }

    #[test]
    fn draw_renders_overview_tools_and_health() {
        let snap = Snapshot {
            since: 0,
            now: 120,
            tools: vec![
                ToolStat {
                    name: "call_path".into(),
                    calls: 5,
                    baseline_tokens: 100_000,
                    returned_tokens: 500,
                    peak_baseline: 40_000,
                    peak_returned: 200,
                    peak_saved: 39_800,
                    last_ts: 110,
                    total_ms: 2500,
                    peak_ms: 800,
                    trunc_count: 0,
                    error_count: 0,
                    low_yield_count: 1,
                },
                // returned >= distilled → should be flagged net-negative.
                ToolStat {
                    name: "include_graph".into(),
                    calls: 2,
                    baseline_tokens: 100,
                    returned_tokens: 400,
                    peak_baseline: 60,
                    peak_returned: 300,
                    peak_saved: 0,
                    last_ts: 90,
                    total_ms: 40,
                    peak_ms: 30,
                    trunc_count: 1,
                    error_count: 0,
                    low_yield_count: 0,
                },
            ],
            events: vec![EventLog {
                ts: 100,
                tool: "call_path".into(),
                baseline: 20_000,
                returned: 120,
                elapsed_ms: 450,
                outcome: "ok".into(),
            }],
        };
        let baseline = session_baseline(&snap);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| draw(f, &snap, &baseline)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(text.contains("wordkeep"));
        assert!(text.contains("dashboard"));
        assert!(text.contains("call_path"));
        assert!(text.contains("include_graph"));
        assert!(text.contains("reduction"));
        assert!(text.contains("session"));
        assert!(text.contains("net-negative")); // include_graph is inverted
        assert!(text.contains("slowest"));
        assert!(text.contains("truncation"));
        assert!(text.contains("peak single-call ms"));
        assert!(text.contains("low-yield"));
    }

    #[test]
    fn session_saved_reflects_baseline_delta() {
        let baseline = session_baseline(&Snapshot {
            since: 0,
            now: 0,
            tools: vec![ToolStat {
                name: "repo_map".into(),
                calls: 2,
                baseline_tokens: 12_000,
                returned_tokens: 600,
                peak_baseline: 0,
                peak_returned: 0,
                peak_saved: 0,
                last_ts: 0,
                total_ms: 0,
                peak_ms: 0,
                trunc_count: 0,
                error_count: 0,
                low_yield_count: 0,
            }],
            events: vec![],
        });
        // 14_000 - 600 = 13_400 saved now; baseline was 11_400 → session delta 2_000.
        assert_eq!(session_saved("repo_map", &baseline, 13_400), 2_000);
    }

    #[test]
    fn draw_handles_empty_snapshot() {
        let snap = Snapshot {
            since: 0,
            now: 0,
            tools: vec![],
            events: vec![],
        };
        let baseline = SessionBaseline::new();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 26)).unwrap();
        terminal.draw(|f| draw(f, &snap, &baseline)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("waiting for tool calls"));
    }
}
