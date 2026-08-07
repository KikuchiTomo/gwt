//! Interactive manager for the secrets manifest — the same shape as the
//! worktree picker, so `git wt secret` feels like `git wt`.
//!
//! The screen is built around the one thing people get wrong: SOURCE and DEST
//! are relative to different roots. Rather than explain that in prose, adding a
//! mapping starts by *picking a real file* out of the repo root, so the source
//! is never typed by hand, and the destination prompt says which root it hangs
//! off.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use gwt_core::layout::BareLayout;
use gwt_core::ops;
use gwt_core::secrets::{self, SecretEntry};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::fuzzy;
use crate::term::{enter_inline, leave_inline};
use crate::theme::{
    fit, frame, highlighted, spinner, title_line, trunc_left, visible_window, C_BRANCH, C_CREATE,
    C_DIM, C_ERR, C_LOCAL, C_PATH, C_POINTER, C_TEXT, PAD, POINTER,
};

/// One manifest row plus the health information the list shows.
struct Row {
    entry: SecretEntry,
    src_exists: bool,
    linked: usize,
}

#[derive(Default, Clone)]
struct Scored {
    idx: usize,
    indices: Vec<usize>,
}

enum Mode {
    List,
    /// Step 1 of add: fuzzy-pick the real file out of the repo root.
    PickSource {
        files: Vec<String>,
        filter: String,
        filtered: Vec<Scored>,
        cursor: usize,
    },
    /// Step 2 of add: type where the link lands inside each worktree.
    TypeDest {
        src: String,
        buf: String,
    },
    ConfirmRemove {
        entry: SecretEntry,
    },
    /// Removing/relinking touches every worktree, so it gets a spinner too.
    Working {
        label: String,
        frame: usize,
        job: Job,
    },
    Message {
        text: String,
        error: bool,
    },
}

enum Job {
    Add { src: String, dst: String },
    Remove { src: String },
    Relink,
}

struct App {
    layout: BareLayout,
    rows: Vec<Row>,
    worktrees: Vec<PathBuf>,
    filter: String,
    filter_active: bool,
    filtered: Vec<Scored>,
    cursor: usize,
    mode: Mode,
}

pub fn run_secrets(layout: &BareLayout) -> Result<()> {
    let mut term = enter_inline(18)?;
    let result = (|| -> Result<()> {
        let mut app = App::new(layout.clone())?;
        loop {
            term.draw(|f| draw(f, &app))?;
            if matches!(app.mode, Mode::Working { .. }) {
                app.tick_work();
                std::thread::sleep(Duration::from_millis(70));
                continue;
            }
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            if let Event::Key(key) = event::read()? {
                if app.handle_key(key)? {
                    return Ok(());
                }
            }
        }
    })();
    leave_inline(&mut term)?;
    result
}

impl App {
    fn new(layout: BareLayout) -> Result<Self> {
        let mut s = Self {
            layout,
            rows: Vec::new(),
            worktrees: Vec::new(),
            filter: String::new(),
            filter_active: false,
            filtered: Vec::new(),
            cursor: 0,
            mode: Mode::List,
        };
        s.reload()?;
        Ok(s)
    }

    fn reload(&mut self) -> Result<()> {
        self.worktrees = ops::worktree_dirs(&self.layout).unwrap_or_default();
        let entries = secrets::read_manifest(&self.layout)?;
        self.rows = entries
            .into_iter()
            .map(|entry| {
                let src_exists = entry.src_abs(&self.layout).exists();
                let linked = ops::secret_link_count(&self.layout, &entry, &self.worktrees);
                Row {
                    entry,
                    src_exists,
                    linked,
                }
            })
            .collect();
        self.refilter();
        Ok(())
    }

    fn refilter(&mut self) {
        let q = self.filter.clone();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| {
                let hay = format!("{} {}", r.entry.src, r.entry.dst);
                fuzzy::score(&q, &hay).map(|m| Scored {
                    idx,
                    indices: m.indices,
                })
            })
            .collect();
        if self.cursor >= self.filtered.len() {
            self.cursor = self.filtered.len().saturating_sub(1);
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.filtered.get(self.cursor)?.idx)
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as isize;
        self.cursor = ((self.cursor as isize) + delta).rem_euclid(len) as usize;
    }

    fn start(&mut self, label: String, job: Job) {
        self.mode = Mode::Working {
            label,
            frame: 0,
            job,
        };
    }

    /// One spinner beat, then run the job. These are local filesystem walks over
    /// a handful of worktrees, so a single blocking beat is honest and simple.
    fn tick_work(&mut self) {
        let (frame_no, ()) = match &mut self.mode {
            Mode::Working { frame, .. } => {
                *frame = frame.wrapping_add(1);
                (*frame, ())
            }
            _ => return,
        };
        if frame_no < 2 {
            return;
        }
        let Mode::Working { job, .. } = &self.mode else {
            return;
        };
        let outcome = match job {
            Job::Add { src, dst } => ops::secret_add(&self.layout, src, dst)
                .map(|r| {
                    let n = r
                        .linked
                        .iter()
                        .filter(|(_, o)| matches!(o, secrets::LinkOutcome::Linked))
                        .count();
                    if r.src_exists {
                        format!(
                            "{} → (worktree)/{}  · linked into {n}",
                            r.entry.src, r.entry.dst
                        )
                    } else {
                        format!(
                            "{} registered, but the source does not exist yet",
                            r.entry.src
                        )
                    }
                })
                .map_err(|e| e.to_string()),
            Job::Remove { src } => ops::secret_remove(&self.layout, src)
                .map_err(|e| e.to_string())
                .and_then(|opt| match opt {
                    None => Err(format!("no entry for {src}")),
                    Some(r) => {
                        let removed = r
                            .unlinked
                            .iter()
                            .filter(|(_, o)| *o == secrets::UnlinkOutcome::Removed)
                            .count();
                        let kept: Vec<String> = r
                            .unlinked
                            .iter()
                            .filter(|(_, o)| matches!(o, secrets::UnlinkOutcome::Kept { .. }))
                            .map(|(p, _)| name_of(p))
                            .collect();
                        let mut msg = format!("removed {} · unlinked {removed}", r.entry.src);
                        if !kept.is_empty() {
                            // A real file where the link was is worth naming.
                            msg.push_str(&format!("  · kept real file in {}", kept.join(", ")));
                        }
                        Ok(msg)
                    }
                }),
            Job::Relink => ops::relink(&self.layout)
                .map(|v| format!("relinked {} worktree(s)", v.len()))
                .map_err(|e| e.to_string()),
        };
        let _ = self.reload();
        self.mode = match outcome {
            Ok(text) => Mode::Message { text, error: false },
            Err(text) => Mode::Message { text, error: true },
        };
    }

    /// Candidate source files: everything under the repo root that is not a
    /// worktree, not the bare dir, and not VCS noise. In practice that is
    /// `secrets/` plus any stray root-level config.
    fn source_candidates(&self) -> Vec<String> {
        let mut out = Vec::new();
        let skip: Vec<PathBuf> = self
            .worktrees
            .iter()
            .cloned()
            .chain(std::iter::once(self.layout.bare_dir.clone()))
            // The manifest is our own bookkeeping, never a secret to link.
            .chain(std::iter::once(self.layout.manifest.clone()))
            .collect();
        collect_files(&self.layout.root, &self.layout.root, &skip, 0, &mut out);
        out.sort();
        // Already-mapped sources go last: the reason you opened `a` is almost
        // always a file that is not linked yet.
        let mapped: Vec<&str> = self.rows.iter().map(|r| r.entry.src.as_str()).collect();
        out.sort_by_key(|f| mapped.contains(&f.as_str()));
        out
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.mode {
            Mode::Working { .. } => Ok(false),
            Mode::Message { .. } => {
                self.mode = Mode::List;
                Ok(false)
            }
            Mode::ConfirmRemove { entry } => {
                let src = entry.src.clone();
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.start(format!("removing {src}"), Job::Remove { src });
                    }
                    _ => self.mode = Mode::List,
                }
                Ok(false)
            }
            Mode::TypeDest { src, buf } => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::List,
                    KeyCode::Char('c') if ctrl => self.mode = Mode::List,
                    KeyCode::Enter => {
                        let (src, dst) = (src.clone(), buf.trim().to_string());
                        if dst.is_empty() {
                            self.mode = Mode::Message {
                                text: "destination is required".into(),
                                error: true,
                            };
                        } else {
                            self.start(format!("linking {src}"), Job::Add { src, dst });
                        }
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => buf.push(c),
                    _ => {}
                }
                Ok(false)
            }
            Mode::PickSource { .. } => {
                self.handle_pick_source(key, ctrl);
                Ok(false)
            }
            Mode::List => self.handle_list(key, ctrl),
        }
    }

    fn handle_pick_source(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => self.mode = Mode::List,
            KeyCode::Down => self.pick_move(1),
            KeyCode::Up => self.pick_move(-1),
            KeyCode::Char('n') if ctrl => self.pick_move(1),
            KeyCode::Char('p') if ctrl => self.pick_move(-1),
            KeyCode::Char('j') if ctrl => self.pick_move(1),
            KeyCode::Char('k') if ctrl => self.pick_move(-1),
            KeyCode::Enter => {
                let chosen = match &self.mode {
                    Mode::PickSource {
                        files,
                        filtered,
                        cursor,
                        ..
                    } => filtered.get(*cursor).map(|s| files[s.idx].clone()),
                    _ => None,
                };
                if let Some(src) = chosen {
                    // Default the destination to the file's own name — the
                    // overwhelmingly common case (secrets/.env -> .env).
                    let buf = Path::new(&src)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.mode = Mode::TypeDest { src, buf };
                }
            }
            KeyCode::Backspace => {
                if let Mode::PickSource { filter, .. } = &mut self.mode {
                    filter.pop();
                }
                self.refilter_sources();
            }
            KeyCode::Char(c) => {
                if let Mode::PickSource { filter, .. } = &mut self.mode {
                    filter.push(c);
                }
                self.refilter_sources();
            }
            _ => {}
        }
    }

    fn pick_move(&mut self, delta: isize) {
        if let Mode::PickSource {
            filtered, cursor, ..
        } = &mut self.mode
        {
            if filtered.is_empty() {
                return;
            }
            let len = filtered.len() as isize;
            *cursor = ((*cursor as isize) + delta).rem_euclid(len) as usize;
        }
    }

    fn refilter_sources(&mut self) {
        if let Mode::PickSource {
            files,
            filter,
            filtered,
            cursor,
        } = &mut self.mode
        {
            *filtered = files
                .iter()
                .enumerate()
                .filter_map(|(idx, f)| {
                    fuzzy::score(filter, f).map(|m| Scored {
                        idx,
                        indices: m.indices,
                    })
                })
                .collect();
            if *cursor >= filtered.len() {
                *cursor = filtered.len().saturating_sub(1);
            }
        }
    }

    fn handle_list(&mut self, key: KeyEvent, ctrl: bool) -> Result<bool> {
        match key.code {
            KeyCode::Down => {
                self.move_cursor(1);
                return Ok(false);
            }
            KeyCode::Up => {
                self.move_cursor(-1);
                return Ok(false);
            }
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => {
                self.move_cursor(1);
                return Ok(false);
            }
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => {
                self.move_cursor(-1);
                return Ok(false);
            }
            KeyCode::Char('c') if ctrl => return Ok(true),
            _ => {}
        }

        if self.filter_active {
            match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filter_active = false;
                    self.refilter();
                }
                KeyCode::Enter => self.filter_active = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.refilter();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.refilter();
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char('g') => self.cursor = 0,
            KeyCode::Char('G') => self.cursor = self.filtered.len().saturating_sub(1),
            KeyCode::Char('a') => {
                let files = self.source_candidates();
                if files.is_empty() {
                    self.mode = Mode::Message {
                        text: format!(
                            "no candidate files under {} (put the real file there first)",
                            self.layout.root.display()
                        ),
                        error: true,
                    };
                } else {
                    self.mode = Mode::PickSource {
                        filtered: (0..files.len())
                            .map(|idx| Scored {
                                idx,
                                indices: Vec::new(),
                            })
                            .collect(),
                        files,
                        filter: String::new(),
                        cursor: 0,
                    };
                }
            }
            KeyCode::Char('d') => {
                if let Some(row) = self.selected() {
                    self.mode = Mode::ConfirmRemove {
                        entry: row.entry.clone(),
                    };
                }
            }
            KeyCode::Char('r') => self.start("relinking".into(), Job::Relink),
            KeyCode::Char('f') | KeyCode::Char('/') => self.filter_active = true,
            _ => {}
        }
        Ok(false)
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Walk `dir` collecting root-relative file paths, skipping worktrees, the bare
/// dir, and `.git`. Depth-capped so a stray huge tree can't stall the picker.
fn collect_files(root: &Path, dir: &Path, skip: &[PathBuf], depth: usize, out: &mut Vec<String>) {
    if depth > 4 || out.len() > 2000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" || skip.contains(&p) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            collect_files(root, &p, skip, depth + 1, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

// ---- rendering ------------------------------------------------------------

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let block = frame(title(app), help(app));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    match &app.mode {
        Mode::PickSource {
            files,
            filtered,
            cursor,
            filter,
        } => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(PAD),
                    Span::styled(
                        "pick the real file — paths are relative to ",
                        Style::default().fg(C_DIM),
                    ),
                    Span::styled(
                        app.layout.root.display().to_string(),
                        Style::default().fg(C_LOCAL),
                    ),
                ])),
                chunks[0],
            );
            draw_sources(f, chunks[1], files, filtered, *cursor);
            draw_prompt(f, chunks[2], "source", filter, true);
        }
        Mode::TypeDest { src, buf } => {
            draw_dest_help(f, chunks[0], chunks[1], src, &app.layout.root);
            draw_prompt(f, chunks[2], "dest (in each worktree)", buf, true);
        }
        _ => {
            draw_header(f, chunks[0], app);
            draw_rows(f, chunks[1], app);
            draw_status(f, chunks[2], app);
        }
    }
}

fn title(app: &App) -> Line<'static> {
    match &app.mode {
        Mode::PickSource {
            filtered, files, ..
        } => title_line(
            "secret · pick source",
            &format!("{}/{}", filtered.len(), files.len()),
        ),
        Mode::TypeDest { .. } => title_line("secret · destination", "in every worktree"),
        _ => title_line(
            "git wt secret",
            &format!("{}/{}", app.filtered.len(), app.rows.len()),
        ),
    }
}

fn help(app: &App) -> Line<'static> {
    let s = match &app.mode {
        Mode::List if app.filter_active => " type:filter  esc:clear  ↑↓:nav  enter:done ",
        Mode::List => " j/k ↑↓:nav  a:add  d:remove  r:relink  f:filter  q:quit ",
        Mode::PickSource { .. } => " type:filter  ↑↓/^p^n:nav  enter:choose file  esc:back ",
        Mode::TypeDest { .. } => " type:path inside each worktree  enter:link now  esc:cancel ",
        Mode::ConfirmRemove { .. } => " y: remove mapping + its links   any other key: cancel ",
        Mode::Working { .. } => " working… ",
        Mode::Message { .. } => " press any key ",
    };
    Line::from(Span::styled(s, Style::default().fg(C_DIM)))
}

fn cols(app: &App) -> (usize, usize) {
    let src = app
        .rows
        .iter()
        .map(|r| r.entry.src.chars().count())
        .chain(std::iter::once(22))
        .max()
        .unwrap_or(22)
        .min(40);
    let dst = app
        .rows
        .iter()
        .map(|r| r.entry.dst.chars().count())
        .chain(std::iter::once(20))
        .max()
        .unwrap_or(20)
        .min(34);
    (src, dst)
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let (sw, dw) = cols(app);
    let style = Style::default()
        .fg(ratatui::style::Color::White)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let spans = vec![
        Span::raw(PAD),
        Span::styled(fit("SOURCE (<repo-root>/…)", sw), style),
        Span::raw(" "),
        Span::styled(fit("DEST (<worktree>/…)", dw), style),
        Span::raw(" "),
        Span::styled(fit("SOURCE", 8), style),
        Span::raw(" "),
        Span::styled("LINKED", style),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_rows(f: &mut Frame, area: Rect, app: &App) {
    if app.rows.is_empty() {
        let lines = vec![
            Line::from(Span::raw("")),
            Line::from(vec![
                Span::raw(PAD),
                Span::styled("no secret mappings yet.", Style::default().fg(C_DIM)),
            ]),
            Line::from(vec![
                Span::raw(PAD),
                Span::styled("press ", Style::default().fg(C_DIM)),
                Span::styled(
                    "a",
                    Style::default().fg(C_CREATE).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " to pick a file and link it into every worktree.",
                    Style::default().fg(C_DIM),
                ),
            ]),
        ];
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    let (sw, dw) = cols(app);
    let cap = area.height as usize;
    let (start, end) = visible_window(app.filtered.len(), app.cursor, cap);
    let lines: Vec<Line> = (start..end)
        .map(|i| {
            let s = &app.filtered[i];
            let r = &app.rows[s.idx];
            let cursor = i == app.cursor;
            let (state, state_color) = if r.src_exists {
                ("ok", C_CREATE)
            } else {
                ("MISSING", C_ERR)
            };
            let total = app.worktrees.len();
            let link_color = if r.linked == total && total > 0 {
                C_CREATE
            } else if r.linked == 0 {
                C_ERR
            } else {
                C_BRANCH
            };
            let mut spans = vec![Span::styled(
                if cursor { POINTER } else { PAD },
                Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
            )];
            let src_cell = fit(&r.entry.src, sw);
            spans.extend(
                highlighted(&src_cell, &s.indices, C_LOCAL)
                    .into_iter()
                    .map(|sp| Span::styled(sp.content.into_owned(), sp.style)),
            );
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                fit(&r.entry.dst, dw),
                Style::default().fg(C_BRANCH),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                fit(state, 8),
                Style::default().fg(state_color),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{}/{}", r.linked, total),
                Style::default().fg(link_color),
            ));
            Line::from(spans).style(if cursor {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_sources(f: &mut Frame, area: Rect, files: &[String], filtered: &[Scored], cursor: usize) {
    let cap = area.height as usize;
    let (start, end) = visible_window(filtered.len(), cursor, cap);
    let lines: Vec<Line> = (start..end)
        .map(|i| {
            let s = &filtered[i];
            let mut spans = vec![Span::styled(
                if i == cursor { POINTER } else { PAD },
                Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
            )];
            spans.extend(highlighted(&files[s.idx], &s.indices, C_LOCAL));
            Line::from(spans).style(if i == cursor {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// Spell out both absolute paths so the two roots are unmistakable.
fn draw_dest_help(f: &mut Frame, top: Rect, body: Rect, src: &str, root: &Path) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(PAD),
            Span::styled("source ", Style::default().fg(C_DIM)),
            Span::styled(
                root.join(src).display().to_string(),
                Style::default().fg(C_LOCAL),
            ),
        ])),
        top,
    );
    let lines = vec![
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::raw(PAD),
            Span::styled(
                "where should the link appear inside each worktree?",
                Style::default().fg(C_BRANCH),
            ),
        ]),
        Line::from(vec![
            Span::raw(PAD),
            Span::styled(
                "the path is relative to that worktree's root, e.g. ",
                Style::default().fg(C_DIM),
            ),
            Span::styled(".env", Style::default().fg(C_CREATE)),
            Span::styled(" or ", Style::default().fg(C_DIM)),
            Span::styled("config/gcp.json", Style::default().fg(C_CREATE)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}

fn draw_prompt(f: &mut Frame, area: Rect, label: &str, value: &str, caret: bool) {
    let mut spans = vec![
        Span::styled(
            format!(" {label} "),
            Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
        ),
        Span::raw("› "),
        Span::raw(value.to_string()),
    ];
    if caret {
        spans.push(Span::styled("▏", Style::default().fg(C_POINTER)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let line = match &app.mode {
        Mode::ConfirmRemove { entry } => Line::from(vec![
            Span::styled(
                " remove ",
                Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "'{}' and unlink (worktree)/{} everywhere ? y/N",
                entry.src, entry.dst
            )),
        ]),
        Mode::Working { label, frame, .. } => Line::from(vec![
            Span::styled(
                format!(" {} ", spinner(*frame)),
                Style::default().fg(C_CREATE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(label.clone(), Style::default().fg(C_BRANCH)),
        ]),
        Mode::Message { text, error } => Line::from(vec![
            Span::styled(
                if *error { " ! " } else { " ✓ " },
                Style::default().fg(if *error { C_ERR } else { C_CREATE }),
            ),
            Span::styled(
                text.clone(),
                Style::default().fg(if *error { C_ERR } else { C_TEXT }),
            ),
        ]),
        _ if app.filter_active => {
            return draw_prompt(f, area, "filter", &app.filter, true);
        }
        _ => Line::from(vec![
            Span::styled(" manifest ", Style::default().fg(C_DIM)),
            Span::styled(
                trunc_left(
                    &app.layout.manifest.display().to_string(),
                    area.width.saturating_sub(12) as usize,
                ),
                Style::default().fg(C_PATH),
            ),
        ]),
    };
    f.render_widget(Paragraph::new(line), area);
}
