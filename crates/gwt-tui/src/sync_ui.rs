//! Interactive manager for the sync recipe — the same shape as the worktree
//! picker, so `git wt sync` feels like `git wt`.
//!
//! The screen is built around the two things people get wrong. SOURCE and DEST
//! are relative to different roots, so adding a file step starts by *picking a
//! real file* out of the repo root and the destination prompt names the other
//! root. And a step is no longer only a symlink, so the kind is chosen first
//! and shown in its own column rather than inferred from the row.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use gwt_core::cache::{self as cache_core, CacheMode, CacheStep};
use gwt_core::layout::BareLayout;
use gwt_core::sync::{
    self, CopyStep, LinkStep, Outcome, Phase, RunStep, Step, UnlinkOutcome, DEFAULT_TIMEOUT,
};
use gwt_core::{ops, t};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::fuzzy;
use crate::term::{enter_inline, leave_inline};
use crate::theme::{
    fit, frame, highlighted, spinner, title_line, trunc_left, visible_window, KeyRow, KeySection,
    C_BRANCH, C_CREATE, C_DIM, C_ERR, C_LOCAL, C_PATH, C_POINTER, C_TEXT, PAD, POINTER,
};

/// One recipe row plus the health information the list shows.
struct Row {
    step: Step,
    /// Position in the recipe — what edit and remove act on. Filtering reorders
    /// the view, so the row cannot be identified by its screen position.
    idx: usize,
    src_exists: bool,
    applied: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Link,
    Copy,
    Run,
    Cache,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Link, Kind::Copy, Kind::Run, Kind::Cache];

    fn key(self) -> char {
        match self {
            Kind::Link => 'l',
            Kind::Copy => 'c',
            Kind::Run => 'r',
            Kind::Cache => 'k',
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Link => "link",
            Kind::Copy => "copy",
            Kind::Run => "run",
            Kind::Cache => "cache",
        }
    }

    fn desc(self) -> &'static str {
        match self {
            Kind::Link => t::kind_link_desc(),
            Kind::Copy => t::kind_copy_desc(),
            Kind::Run => t::kind_run_desc(),
            Kind::Cache => t::kind_cache_desc(),
        }
    }
}

#[derive(Default, Clone)]
struct Scored {
    idx: usize,
    indices: Vec<usize>,
}

enum Mode {
    List,
    /// Step 0 of add: link, copy or run?
    PickKind {
        cursor: usize,
    },
    /// Step 1 of a file step: fuzzy-pick the real file out of the repo root.
    PickSource {
        kind: Kind,
        files: Vec<String>,
        filter: String,
        filtered: Vec<Scored>,
        cursor: usize,
    },
    /// Step 2 of a file step: type where it lands inside each worktree.
    /// Also reached by `e`, which edits an existing step in place.
    TypeDest {
        kind: Kind,
        src: String,
        buf: String,
        overwrite: bool,
        render: bool,
        editing: Option<usize>,
    },
    /// The `run` equivalent: type the command line.
    TypeCommand {
        buf: String,
        existing: Option<RunStep>,
        editing: Option<usize>,
    },
    /// A cache takes three answers, so it takes three screens, each shaped like
    /// one that already exists rather than one dense form.
    CachePath {
        buf: String,
        existing: Option<CacheStep>,
        editing: Option<usize>,
    },
    CacheSharing {
        path: String,
        cursor: usize,
        existing: Option<CacheStep>,
        editing: Option<usize>,
    },
    CacheKey {
        path: String,
        mode: CacheMode,
        buf: String,
        existing: Option<CacheStep>,
        editing: Option<usize>,
    },
    /// The `?` overlay.
    Keys {
        scroll: u16,
    },
    ConfirmRemove {
        idx: usize,
        step: Step,
    },
    /// Removing or applying touches every worktree, so it gets a spinner too.
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
    Add(Step),
    Replace(usize, Step),
    Remove(usize),
    Apply,
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

pub fn run_sync_manager(layout: &BareLayout) -> Result<()> {
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
        let steps = sync::load(&self.layout)?.steps;
        self.rows = steps
            .into_iter()
            .enumerate()
            .map(|(idx, step)| {
                let src_exists = step.src_abs(&self.layout).is_none_or(|p| p.exists());
                let applied = ops::sync_applied_count(&self.layout, &step, &self.worktrees);
                Row {
                    step,
                    idx,
                    src_exists,
                    applied,
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
                let hay = format!(
                    "{} {} {}",
                    r.step.kind(),
                    r.step.subject(),
                    r.step.dst().unwrap_or("")
                );
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
            Job::Add(step) => ops::sync_add(&self.layout, step.clone())
                .map(|r| added_message(&r))
                .map_err(|e| e.to_string()),
            Job::Replace(idx, step) => ops::sync_replace_at(&self.layout, *idx, step.clone())
                .map_err(|e| e.to_string())
                .and_then(|opt| match opt {
                    None => Err(t::sync_no_entry(&step.subject())),
                    Some(r) => Ok(added_message(&r)),
                }),
            Job::Remove(idx) => ops::sync_remove_at(&self.layout, *idx)
                .map_err(|e| e.to_string())
                .and_then(|opt| match opt {
                    None => Err(t::sync_no_entry("")),
                    Some(r) => {
                        let removed = r
                            .unlinked
                            .iter()
                            .filter(|(_, o)| *o == UnlinkOutcome::Removed)
                            .count();
                        let kept: Vec<String> = r
                            .unlinked
                            .iter()
                            .filter(|(_, o)| matches!(o, UnlinkOutcome::Kept { .. }))
                            .map(|(p, _)| name_of(p))
                            .collect();
                        let mut msg = t::sync_removed(&r.step.subject(), removed);
                        if !kept.is_empty() {
                            // A real file where the link was is worth naming.
                            msg.push_str(&t::sync_kept_real(&kept.join(", ")));
                        }
                        Ok(msg)
                    }
                }),
            // The manager repairs files; it does not re-run anyone's build.
            Job::Apply => ops::sync_apply(&self.layout, Phase::Apply, &mut sync::noop)
                .map(|v| t::sync_applied_to(v.len()))
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
            // Our own bookkeeping, never a file to hand to a worktree.
            .chain(std::iter::once(self.layout.gwt_dir.clone()))
            .chain(std::iter::once(self.layout.legacy_manifest.clone()))
            .collect();
        collect_files(&self.layout.root, &self.layout.root, &skip, 0, &mut out);
        out.sort();
        // Already-used sources go last: the reason you opened `a` is almost
        // always a file that is not in the recipe yet.
        let mapped: Vec<&str> = self.rows.iter().filter_map(|r| r.step.src()).collect();
        out.sort_by_key(|f| mapped.contains(&f.as_str()));
        out
    }

    fn key_help() -> Vec<KeySection> {
        vec![
            KeySection {
                title: t::help_sec_nav().into(),
                rows: vec![
                    KeyRow {
                        keys: "j / k   ↑ / ↓",
                        desc: t::k_updown().into(),
                    },
                    KeyRow {
                        keys: "g / G",
                        desc: t::k_topbottom().into(),
                    },
                    KeyRow {
                        keys: "f  /",
                        desc: t::k_filter().into(),
                    },
                ],
            },
            KeySection {
                title: t::help_sec_act().into(),
                rows: vec![
                    KeyRow {
                        keys: "a",
                        desc: t::k_sadd().into(),
                    },
                    KeyRow {
                        keys: "e",
                        desc: t::k_sedit().into(),
                    },
                    KeyRow {
                        keys: "r",
                        desc: t::k_sapply().into(),
                    },
                ],
            },
            KeySection {
                title: t::help_sec_danger().into(),
                rows: vec![KeyRow {
                    keys: "d",
                    desc: t::k_sdel().into(),
                }],
            },
            KeySection {
                title: t::help_sec_other().into(),
                rows: vec![
                    KeyRow {
                        keys: "?",
                        desc: t::k_help().into(),
                    },
                    KeyRow {
                        keys: "q  esc",
                        desc: t::k_squit().into(),
                    },
                ],
            },
        ]
    }

    /// Which worktrees currently carry the selected step, and which don't.
    fn detail(&self) -> Option<(Vec<String>, Vec<String>)> {
        let row = self.selected()?;
        let (mut have, mut missing) = (Vec::new(), Vec::new());
        for wt in &self.worktrees {
            if sync::is_applied(&self.layout, wt, &row.step) {
                have.push(name_of(wt));
            } else {
                missing.push(name_of(wt));
            }
        }
        Some((have, missing))
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.mode {
            Mode::Working { .. } => Ok(false),
            Mode::Keys { scroll } => {
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
                    _ => self.mode = Mode::List,
                }
                Ok(false)
            }
            Mode::Message { .. } => {
                self.mode = Mode::List;
                Ok(false)
            }
            Mode::ConfirmRemove { idx, step } => {
                let (idx, subject) = (*idx, step.subject().to_string());
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.start(
                            format!("{} {subject}", t::label_removing()),
                            Job::Remove(idx),
                        );
                    }
                    _ => self.mode = Mode::List,
                }
                Ok(false)
            }
            Mode::PickKind { .. } => {
                self.handle_pick_kind(key, ctrl);
                Ok(false)
            }
            Mode::TypeCommand { .. } => {
                self.handle_type_command(key, ctrl);
                Ok(false)
            }
            Mode::CachePath { .. } => {
                self.handle_cache_path(key, ctrl);
                Ok(false)
            }
            Mode::CacheSharing { .. } => {
                self.handle_cache_mode(key, ctrl);
                Ok(false)
            }
            Mode::CacheKey { .. } => {
                self.handle_cache_key(key, ctrl);
                Ok(false)
            }
            Mode::TypeDest { .. } => {
                self.handle_type_dest(key, ctrl);
                Ok(false)
            }
            Mode::PickSource { .. } => {
                self.handle_pick_source(key, ctrl);
                Ok(false)
            }
            Mode::List => self.handle_list(key, ctrl),
        }
    }

    fn handle_pick_kind(&mut self, key: KeyEvent, ctrl: bool) {
        let chosen = match key.code {
            // Nothing is being typed here, so `q` may back out like it does in
            // the list — `esc` alone would be a trap for a picker with no input.
            KeyCode::Esc | KeyCode::Char('q') => return self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => return self.mode = Mode::List,
            KeyCode::Down | KeyCode::Char('j') => return self.kind_move(1),
            KeyCode::Up | KeyCode::Char('k') => return self.kind_move(-1),
            KeyCode::Enter => match &self.mode {
                Mode::PickKind { cursor } => Kind::ALL[*cursor],
                _ => return,
            },
            KeyCode::Char(c) => match Kind::ALL.iter().find(|k| k.key() == c) {
                Some(k) => *k,
                None => return,
            },
            _ => return,
        };
        self.begin_add(chosen);
    }

    fn kind_move(&mut self, delta: isize) {
        if let Mode::PickKind { cursor } = &mut self.mode {
            let len = Kind::ALL.len() as isize;
            *cursor = ((*cursor as isize) + delta).rem_euclid(len) as usize;
        }
    }

    fn begin_add(&mut self, kind: Kind) {
        if kind == Kind::Run {
            self.mode = Mode::TypeCommand {
                buf: String::new(),
                existing: None,
                editing: None,
            };
            return;
        }
        if kind == Kind::Cache {
            // Offer whatever the project obviously needs — a Rust repo means
            // `target` — so the common case is Enter three times.
            let taken: Vec<&str> = self.rows.iter().filter_map(|r| r.step.dst()).collect();
            let suggestion = self
                .presets()
                .into_iter()
                .find(|p| !taken.contains(&p.path.as_str()));
            self.mode = Mode::CachePath {
                buf: suggestion
                    .as_ref()
                    .map(|p| p.path.clone())
                    .unwrap_or_default(),
                existing: suggestion,
                editing: None,
            };
            return;
        }
        let files = self.source_candidates();
        if files.is_empty() {
            self.mode = Mode::Message {
                text: t::no_candidates().into(),
                error: true,
            };
            return;
        }
        self.mode = Mode::PickSource {
            kind,
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

    /// What `git wt cache init` would suggest, read from the default worktree.
    fn presets(&self) -> Vec<CacheStep> {
        let probe = self
            .worktrees
            .iter()
            .find(|w| w.file_name().is_some_and(|n| n == "default"))
            .or_else(|| self.worktrees.first());
        probe.map(|p| cache_core::presets(p)).unwrap_or_default()
    }

    fn handle_cache_path(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => self.mode = Mode::List,
            KeyCode::Enter => {
                let Mode::CachePath {
                    buf,
                    existing,
                    editing,
                } = &self.mode
                else {
                    return;
                };
                let path = match sync::normalize_dst(buf.trim()) {
                    Ok(p) => p,
                    Err(e) => {
                        self.mode = Mode::Message {
                            text: e.to_string(),
                            error: true,
                        };
                        return;
                    }
                };
                let cursor = existing
                    .as_ref()
                    .and_then(|c| CacheMode::ALL.iter().position(|m| *m == c.mode))
                    .unwrap_or(0);
                self.mode = Mode::CacheSharing {
                    path,
                    cursor,
                    existing: existing.clone(),
                    editing: *editing,
                };
            }
            KeyCode::Backspace => {
                if let Mode::CachePath { buf, .. } = &mut self.mode {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::CachePath { buf, .. } = &mut self.mode {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn handle_cache_mode(&mut self, key: KeyEvent, ctrl: bool) {
        let chosen = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => return self.mode = Mode::List,
            KeyCode::Down | KeyCode::Char('j') => {
                if let Mode::CacheSharing { cursor, .. } = &mut self.mode {
                    *cursor = (*cursor + 1) % CacheMode::ALL.len();
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Mode::CacheSharing { cursor, .. } = &mut self.mode {
                    *cursor = (*cursor + CacheMode::ALL.len() - 1) % CacheMode::ALL.len();
                }
                return;
            }
            KeyCode::Enter => match &self.mode {
                Mode::CacheSharing { cursor, .. } => CacheMode::ALL[*cursor],
                _ => return,
            },
            KeyCode::Char(c) => {
                match CacheMode::ALL
                    .iter()
                    .find(|m| m.as_str().starts_with(c.to_ascii_lowercase()))
                {
                    Some(m) => *m,
                    None => return,
                }
            }
            _ => return,
        };
        let Mode::CacheSharing {
            path,
            existing,
            editing,
            ..
        } = &self.mode
        else {
            return;
        };
        let (path, existing, editing) = (path.clone(), existing.clone(), *editing);
        // Only `keyed` has anything left to ask.
        if chosen != CacheMode::Keyed {
            return self.commit_cache(path, chosen, Vec::new(), existing, editing);
        }
        self.mode = Mode::CacheKey {
            buf: existing
                .as_ref()
                .map(|c| c.key.join(" "))
                .unwrap_or_default(),
            path,
            mode: chosen,
            existing,
            editing,
        };
    }

    fn handle_cache_key(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => self.mode = Mode::List,
            KeyCode::Enter => {
                let Mode::CacheKey {
                    path,
                    mode,
                    buf,
                    existing,
                    editing,
                } = &self.mode
                else {
                    return;
                };
                let keys: Vec<String> = buf.split_whitespace().map(str::to_string).collect();
                if keys.is_empty() {
                    self.mode = Mode::Message {
                        text: t::cache_key_required().into(),
                        error: true,
                    };
                    return;
                }
                let (path, mode, existing, editing) =
                    (path.clone(), *mode, existing.clone(), *editing);
                self.commit_cache(path, mode, keys, existing, editing);
            }
            KeyCode::Backspace => {
                if let Mode::CacheKey { buf, .. } = &mut self.mode {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::CacheKey { buf, .. } = &mut self.mode {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn commit_cache(
        &mut self,
        path: String,
        mode: CacheMode,
        key: Vec<String>,
        existing: Option<CacheStep>,
        editing: Option<usize>,
    ) {
        // seed and env are not on any of these screens; carrying them over
        // means editing a mode never silently drops them.
        let step = Step::Cache(CacheStep {
            path,
            mode,
            key,
            seed: existing.as_ref().map(|c| c.seed).unwrap_or(true),
            env: existing.as_ref().and_then(|c| c.env.clone()),
        });
        let label = t::label_mounting().to_string();
        match editing {
            Some(i) => self.start(label, Job::Replace(i, step)),
            None => self.start(label, Job::Add(step)),
        }
    }

    fn handle_type_command(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => self.mode = Mode::List,
            KeyCode::Enter => {
                let Mode::TypeCommand {
                    buf,
                    existing,
                    editing,
                } = &self.mode
                else {
                    return;
                };
                let cmd = buf.trim().to_string();
                if cmd.is_empty() {
                    self.mode = Mode::Message {
                        text: t::cmd_required().into(),
                        error: true,
                    };
                    return;
                }
                // Editing keeps only_if/timeout/dir, which this screen does not
                // show: losing them because a typo was fixed would be worse
                // than not being able to set them here at all.
                let step = Step::Run(match existing {
                    Some(prev) => RunStep {
                        cmd,
                        ..prev.clone()
                    },
                    None => RunStep {
                        cmd,
                        when: vec![Phase::Create],
                        only_if: None,
                        timeout: DEFAULT_TIMEOUT,
                        dir: None,
                    },
                });
                let label = t::label_linking().to_string();
                match editing {
                    Some(i) => self.start(label, Job::Replace(*i, step)),
                    None => self.start(label, Job::Add(step)),
                }
            }
            KeyCode::Backspace => {
                if let Mode::TypeCommand { buf, .. } = &mut self.mode {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::TypeCommand { buf, .. } = &mut self.mode {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn handle_type_dest(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Char('c') if ctrl => self.mode = Mode::List,
            // The two copy flags, where the copy is being described.
            KeyCode::Char('o') if ctrl => {
                if let Mode::TypeDest {
                    kind, overwrite, ..
                } = &mut self.mode
                {
                    if *kind == Kind::Copy {
                        *overwrite = !*overwrite;
                    }
                }
            }
            KeyCode::Char('r') if ctrl => {
                if let Mode::TypeDest { kind, render, .. } = &mut self.mode {
                    if *kind == Kind::Copy {
                        *render = !*render;
                    }
                }
            }
            KeyCode::Enter => {
                let Mode::TypeDest {
                    kind,
                    src,
                    buf,
                    overwrite,
                    render,
                    editing,
                } = &self.mode
                else {
                    return;
                };
                let dst = buf.trim().to_string();
                if dst.is_empty() {
                    self.mode = Mode::Message {
                        text: t::dest_required().into(),
                        error: true,
                    };
                    return;
                }
                let dst = match sync::normalize_dst(&dst) {
                    Ok(d) => d,
                    Err(e) => {
                        self.mode = Mode::Message {
                            text: e.to_string(),
                            error: true,
                        };
                        return;
                    }
                };
                let step = match kind {
                    Kind::Link => Step::Link(LinkStep {
                        src: src.clone(),
                        dst,
                    }),
                    _ => Step::Copy(CopyStep {
                        src: src.clone(),
                        dst,
                        overwrite: *overwrite,
                        render: *render,
                    }),
                };
                let label = format!("{} {src}", t::label_linking());
                match editing {
                    Some(i) => self.start(label, Job::Replace(*i, step)),
                    None => self.start(label, Job::Add(step)),
                }
            }
            KeyCode::Backspace => {
                if let Mode::TypeDest { buf, .. } = &mut self.mode {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::TypeDest { buf, .. } = &mut self.mode {
                    buf.push(c);
                }
            }
            _ => {}
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
                        kind,
                        files,
                        filtered,
                        cursor,
                        ..
                    } => filtered.get(*cursor).map(|s| (*kind, files[s.idx].clone())),
                    _ => None,
                };
                if let Some((kind, src)) = chosen {
                    // Default the destination to the file's own name — the
                    // overwhelmingly common case (secrets/.env -> .env).
                    let buf = Path::new(&src)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.mode = Mode::TypeDest {
                        kind,
                        src,
                        buf,
                        overwrite: false,
                        render: false,
                        editing: None,
                    };
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
            ..
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
            KeyCode::Char('a') => self.mode = Mode::PickKind { cursor: 0 },
            KeyCode::Char('d') => {
                if let Some(row) = self.selected() {
                    self.mode = Mode::ConfirmRemove {
                        idx: row.idx,
                        step: row.step.clone(),
                    };
                }
            }
            KeyCode::Char('e') => {
                // Edit in place: the same prompt, pre-filled, so a typo is a
                // two-key fix rather than remove + re-add.
                if let Some(row) = self.selected() {
                    self.mode = match &row.step {
                        Step::Run(r) => Mode::TypeCommand {
                            buf: r.cmd.clone(),
                            existing: Some(r.clone()),
                            editing: Some(row.idx),
                        },
                        Step::Link(l) => Mode::TypeDest {
                            kind: Kind::Link,
                            src: l.src.clone(),
                            buf: l.dst.clone(),
                            overwrite: false,
                            render: false,
                            editing: Some(row.idx),
                        },
                        Step::Copy(c) => Mode::TypeDest {
                            kind: Kind::Copy,
                            src: c.src.clone(),
                            buf: c.dst.clone(),
                            overwrite: c.overwrite,
                            render: c.render,
                            editing: Some(row.idx),
                        },
                        Step::Cache(c) => Mode::CachePath {
                            buf: c.path.clone(),
                            existing: Some(c.clone()),
                            editing: Some(row.idx),
                        },
                    };
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Keys { scroll: 0 },
            KeyCode::Char('r') => self.start(t::label_applying().into(), Job::Apply),
            KeyCode::Char('f') | KeyCode::Char('/') => self.filter_active = true,
            _ => {}
        }
        Ok(false)
    }
}

fn added_message(r: &ops::StepAdded) -> String {
    match &r.step {
        Step::Run(run) => t::sync_registered_cmd(&run.cmd),
        step => {
            if r.src_exists {
                let n = r
                    .applied
                    .iter()
                    .filter(|(_, o)| matches!(o, Outcome::Linked | Outcome::Copied))
                    .count();
                t::sync_applied_into(&step.subject(), step.dst().unwrap_or(""), n)
            } else {
                t::sync_registered_no_src(&step.subject())
            }
        }
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
        Mode::PickKind { cursor } => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(PAD),
                    Span::styled(t::choose_hint(), Style::default().fg(C_DIM)),
                ])),
                chunks[0],
            );
            draw_kinds(f, chunks[1], *cursor);
            draw_status(f, chunks[2], app);
        }
        Mode::PickSource {
            files,
            filtered,
            cursor,
            filter,
            ..
        } => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(PAD),
                    Span::styled(t::pick_source_hint(), Style::default().fg(C_DIM)),
                    Span::styled(
                        app.layout.root.display().to_string(),
                        Style::default().fg(C_LOCAL),
                    ),
                ])),
                chunks[0],
            );
            draw_sources(f, chunks[1], files, filtered, *cursor);
            draw_prompt(f, chunks[2], t::label_source(), filter, true);
        }
        Mode::TypeDest {
            kind,
            src,
            buf,
            overwrite,
            render,
            ..
        } => {
            draw_dest_help(
                f,
                chunks[0],
                chunks[1],
                *kind,
                src,
                &app.layout.root,
                *overwrite,
                *render,
            );
            draw_prompt(f, chunks[2], t::label_dest(), buf, true);
        }
        Mode::TypeCommand { buf, .. } => {
            draw_cmd_help(f, chunks[0], chunks[1]);
            draw_prompt(f, chunks[2], t::label_command(), buf, true);
        }
        Mode::CachePath { buf, .. } => {
            draw_lines(
                f,
                chunks[0],
                chunks[1],
                t::cache_path_question(),
                &[
                    (t::cache_path_hint(), "target"),
                    (t::cache_path_hint2(), ".gwt/cache"),
                ],
            );
            draw_prompt(f, chunks[2], t::label_cache_path(), buf, true);
        }
        Mode::CacheSharing { path, cursor, .. } => {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(PAD),
                    Span::styled(
                        format!("{} ", t::label_cache_path()),
                        Style::default().fg(C_DIM),
                    ),
                    Span::styled(path.clone(), Style::default().fg(C_LOCAL)),
                ])),
                chunks[0],
            );
            draw_cache_modes(f, chunks[1], *cursor);
            draw_status(f, chunks[2], app);
        }
        Mode::CacheKey { path, buf, .. } => {
            draw_lines(
                f,
                chunks[0],
                chunks[1],
                &t::cache_key_question(path),
                &[
                    (t::cache_key_hint(), "Cargo.lock"),
                    (t::cache_key_hint2(), "package-lock.json"),
                ],
            );
            draw_prompt(f, chunks[2], t::label_cache_key(), buf, true);
        }
        Mode::Keys { scroll } => crate::theme::draw_keys(f, inner, &App::key_help(), *scroll),
        _ => {
            draw_header(f, chunks[0], app);
            // A detail strip under the list turns "2/3" into the actual names.
            let detail_h = if app.rows.is_empty() { 0 } else { 2 };
            let body = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(detail_h)])
                .split(chunks[1]);
            draw_rows(f, body[0], app);
            if detail_h > 0 {
                draw_detail(f, body[1], app);
            }
            draw_status(f, chunks[2], app);
        }
    }
}

fn title(app: &App) -> Line<'static> {
    match &app.mode {
        Mode::PickKind { .. } => title_line(t::sync_title_kind(), ""),
        Mode::PickSource {
            filtered, files, ..
        } => title_line(
            t::sync_title_source(),
            &format!("{}/{}", filtered.len(), files.len()),
        ),
        Mode::TypeDest { .. } => title_line(t::sync_title_dest(), t::sync_dest_sub()),
        Mode::TypeCommand { .. } => title_line(t::sync_title_cmd(), t::sync_dest_sub()),
        Mode::CachePath { .. } | Mode::CacheSharing { .. } | Mode::CacheKey { .. } => {
            title_line(t::sync_title_cache(), t::sync_dest_sub())
        }
        _ => title_line(
            t::sync_title(),
            &format!("{}/{}", app.filtered.len(), app.rows.len()),
        ),
    }
}

fn help(app: &App) -> Line<'static> {
    let s = match &app.mode {
        Mode::List if app.filter_active => t::sync_help_filter(),
        Mode::List => t::sync_help(),
        Mode::PickKind { .. } => t::sync_help_kind(),
        Mode::PickSource { .. } => t::sync_help_source(),
        Mode::TypeDest { kind, .. } if *kind == Kind::Copy => t::sync_help_dest_copy(),
        Mode::TypeDest { .. } => t::sync_help_dest(),
        Mode::TypeCommand { .. } => t::sync_help_cmd(),
        Mode::CachePath { .. } => t::sync_help_cache_path(),
        Mode::CacheSharing { .. } => t::sync_help_cache_mode(),
        Mode::CacheKey { .. } => t::sync_help_cache_key(),
        Mode::ConfirmRemove { .. } => t::sync_help_remove(),
        Mode::Working { .. } => t::working(),
        Mode::Keys { .. } => t::help_close(),
        Mode::Message { .. } => t::press_any_key(),
    };
    Line::from(Span::styled(s, Style::default().fg(C_DIM)))
}

const KIND_W: usize = 5;

fn cols(app: &App) -> (usize, usize) {
    let src = app
        .rows
        .iter()
        .map(|r| r.step.subject().chars().count())
        .chain(std::iter::once(22))
        .max()
        .unwrap_or(22)
        .min(40);
    let dst = app
        .rows
        .iter()
        .map(|r| r.step.dst().unwrap_or("").chars().count())
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
        Span::styled(fit(t::col_kind(), KIND_W), style),
        Span::raw(" "),
        Span::styled(fit(t::col_source(), sw), style),
        Span::raw(" "),
        Span::styled(fit(t::col_dest(), dw), style),
        Span::raw(" "),
        Span::styled(fit(t::col_state(), 8), style),
        Span::raw(" "),
        Span::styled(t::col_applied(), style),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn kind_color(step: &Step) -> ratatui::style::Color {
    match step {
        Step::Link(_) => C_LOCAL,
        Step::Copy(_) => C_BRANCH,
        Step::Run(_) => C_CREATE,
        Step::Cache(_) => C_PATH,
    }
}

fn draw_rows(f: &mut Frame, area: Rect, app: &App) {
    if app.rows.is_empty() {
        let lines = vec![
            Line::from(Span::raw("")),
            Line::from(vec![
                Span::raw(PAD),
                Span::styled(t::empty_title(), Style::default().fg(C_DIM)),
            ]),
            Line::from(vec![
                Span::raw(PAD),
                Span::styled(t::empty_hint_pre(), Style::default().fg(C_DIM)),
                Span::styled(
                    "a",
                    Style::default().fg(C_CREATE).add_modifier(Modifier::BOLD),
                ),
                Span::styled(t::empty_hint_post(), Style::default().fg(C_DIM)),
            ]),
        ];
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    let (sw, dw) = cols(app);
    let cap = area.height as usize;
    let (start, end) = visible_window(app.filtered.len(), app.cursor, cap);
    let total = app.worktrees.len();
    let lines: Vec<Line> = (start..end)
        .map(|i| {
            let s = &app.filtered[i];
            let r = &app.rows[s.idx];
            let cursor = i == app.cursor;
            let is_run = matches!(r.step, Step::Run(_));
            let (state, state_color) = match &r.step {
                // A command has no source to be missing; show when it fires.
                Step::Run(run) => (
                    run.when
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    C_DIM,
                ),
                Step::Cache(c) => (c.mode.as_str().to_string(), C_PATH),
                _ if r.src_exists => (t::state_ok().to_string(), C_CREATE),
                _ => (t::state_missing().to_string(), C_ERR),
            };
            let applied_color = if r.applied == total && total > 0 {
                C_CREATE
            } else if r.applied == 0 {
                C_ERR
            } else {
                C_BRANCH
            };
            let mut spans = vec![Span::styled(
                if cursor { POINTER } else { PAD },
                Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
            )];
            spans.push(Span::styled(
                fit(r.step.kind(), KIND_W),
                Style::default().fg(kind_color(&r.step)),
            ));
            spans.push(Span::raw(" "));
            let src_cell = fit(&r.step.subject(), sw);
            spans.extend(
                highlighted(&src_cell, &s.indices, C_LOCAL)
                    .into_iter()
                    .map(|sp| Span::styled(sp.content.into_owned(), sp.style)),
            );
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                fit(r.step.dst().unwrap_or("-"), dw),
                Style::default().fg(C_BRANCH),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                fit(&state, 8),
                Style::default().fg(state_color),
            ));
            spans.push(Span::raw(" "));
            spans.push(if is_run {
                Span::styled("-", Style::default().fg(C_DIM))
            } else {
                Span::styled(
                    format!("{}/{}", r.applied, total),
                    Style::default().fg(applied_color),
                )
            });
            Line::from(spans).style(if cursor {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// Name the worktrees behind the APPLIED count, and flag anything odd about the
/// source — a count alone doesn't tell you which worktree to go fix.
fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(row) = app.selected() else { return };

    if let Step::Run(run) = &row.step {
        return draw_run_detail(f, area, run);
    }
    if let Step::Cache(c) = &row.step {
        return draw_cache_detail(f, area, app, c);
    }
    let Some((have, missing)) = app.detail() else {
        return;
    };
    let label = format!("{} ", t::label_source());
    // Keep the tail: the file name is the identifying part of a long path.
    let budget = (area.width as usize).saturating_sub(crate::theme::width(&label) + 4);
    let src_abs = row
        .step
        .src_abs(&app.layout)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut top = vec![
        Span::raw(PAD),
        Span::styled(label, Style::default().fg(C_DIM)),
        Span::styled(
            trunc_left(&src_abs, budget),
            Style::default().fg(if row.src_exists { C_LOCAL } else { C_ERR }),
        ),
    ];
    if !row.src_exists {
        top.push(Span::styled(
            format!("  ({})", t::src_missing_note()),
            Style::default().fg(C_ERR),
        ));
    }

    let mut bottom = vec![Span::raw(PAD)];
    if app.worktrees.is_empty() {
        bottom.push(Span::styled(
            t::detail_no_worktrees(),
            Style::default().fg(C_DIM),
        ));
    } else {
        if !have.is_empty() {
            bottom.push(Span::styled("✓ ", Style::default().fg(C_CREATE)));
            bottom.push(Span::styled(
                format!("{} {}", t::detail_applied_in(), have.join(", ")),
                Style::default().fg(C_TEXT),
            ));
        }
        if !missing.is_empty() {
            if !have.is_empty() {
                bottom.push(Span::raw("   "));
            }
            bottom.push(Span::styled("✗ ", Style::default().fg(C_ERR)));
            bottom.push(Span::styled(
                format!("{} {}", t::detail_missing_in(), missing.join(", ")),
                Style::default().fg(C_ERR),
            ));
        }
    }
    f.render_widget(
        Paragraph::new(vec![Line::from(top), Line::from(bottom)]),
        area,
    );
}

/// The count alone cannot say what matters about a cache, which is *which*
/// worktrees ended up sharing a bucket and how big it grew.
fn draw_cache_detail(f: &mut Frame, area: Rect, app: &App, c: &CacheStep) {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for wt in &app.worktrees {
        let id = cache_core::bucket_id(c, wt);
        let name = name_of(wt);
        match groups.iter_mut().find(|(g, _)| *g == id) {
            Some((_, names)) => names.push(name),
            None => groups.push((id, vec![name])),
        }
    }

    let mut top = vec![Span::raw(PAD)];
    if groups.is_empty() {
        top.push(Span::styled(
            t::detail_no_worktrees(),
            Style::default().fg(C_DIM),
        ));
    }
    for (id, names) in &groups {
        let bytes = cache_core::dir_size(&cache_core::slot_dir(&app.layout, c).join(id));
        top.push(Span::styled(
            format!("{} ", &id[..id.len().min(8)]),
            Style::default().fg(C_PATH).add_modifier(Modifier::BOLD),
        ));
        top.push(Span::styled(
            format!("{}  ", names.join(", ")),
            Style::default().fg(C_TEXT),
        ));
        top.push(Span::styled(
            format!("{}   ", cache_core::human_bytes(bytes)),
            Style::default().fg(C_DIM),
        ));
    }

    let mut bottom = vec![Span::raw(PAD)];
    // Two buckets means the key genuinely separated something, which is the
    // question anyone looking at this row is asking.
    bottom.push(Span::styled(
        if groups.len() > 1 {
            t::cache_detail_split()
        } else {
            t::cache_detail_together()
        },
        Style::default().fg(if groups.len() > 1 { C_BRANCH } else { C_DIM }),
    ));
    if let Some(env) = &c.env {
        bottom.push(Span::styled(
            format!("   {env}"),
            Style::default().fg(C_LOCAL),
        ));
    }
    f.render_widget(
        Paragraph::new(vec![Line::from(top), Line::from(bottom)]),
        area,
    );
}

/// A command has no per-worktree state to report, so the strip shows the
/// settings that decide whether it fires — the ones only the TOML can set.
fn draw_run_detail(f: &mut Frame, area: Rect, run: &RunStep) {
    let field = |label: &str, value: String, color: ratatui::style::Color| {
        vec![
            Span::styled(format!("{label} "), Style::default().fg(C_DIM)),
            Span::styled(value, Style::default().fg(color)),
            Span::raw("   "),
        ]
    };
    let mut top = vec![Span::raw(PAD)];
    top.extend(field(
        t::detail_runs_on(),
        run.when
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        C_BRANCH,
    ));
    top.extend(field(
        t::detail_timeout(),
        format!("{}s", run.timeout.as_secs()),
        C_TEXT,
    ));
    if let Some(dir) = &run.dir {
        top.extend(field("dir", dir.clone(), C_PATH));
    }

    let mut bottom = vec![Span::raw(PAD)];
    match &run.only_if {
        Some(cond) => bottom.extend(field(t::detail_only_if(), cond.clone(), C_LOCAL)),
        None => bottom.push(Span::styled(
            t::cmd_more_in_toml(),
            Style::default().fg(C_DIM),
        )),
    }
    f.render_widget(
        Paragraph::new(vec![Line::from(top), Line::from(bottom)]),
        area,
    );
}

fn draw_kinds(f: &mut Frame, area: Rect, cursor: usize) {
    let lines: Vec<Line> = Kind::ALL
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let sel = i == cursor;
            Line::from(vec![
                Span::styled(
                    if sel { POINTER } else { PAD },
                    Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}] ", k.key()),
                    Style::default().fg(C_CREATE).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    fit(k.label(), 6),
                    Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(k.desc(), Style::default().fg(C_DIM)),
            ])
            .style(if sel {
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
#[allow(clippy::too_many_arguments)]
fn draw_dest_help(
    f: &mut Frame,
    top: Rect,
    body: Rect,
    kind: Kind,
    src: &str,
    root: &Path,
    overwrite: bool,
    render: bool,
) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(PAD),
            Span::styled(
                format!("{} ", t::label_source()),
                Style::default().fg(C_DIM),
            ),
            Span::styled(
                root.join(src).display().to_string(),
                Style::default().fg(C_LOCAL),
            ),
        ])),
        top,
    );
    let mut lines = vec![
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::raw(PAD),
            Span::styled(t::dest_question(), Style::default().fg(C_BRANCH)),
        ]),
        Line::from(vec![
            Span::raw(PAD),
            Span::styled(t::dest_relative_hint(), Style::default().fg(C_DIM)),
            Span::styled(".env", Style::default().fg(C_CREATE)),
            Span::styled(" / ", Style::default().fg(C_DIM)),
            Span::styled("config/gcp.json", Style::default().fg(C_CREATE)),
        ]),
    ];
    if kind == Kind::Copy {
        let chip = |on: bool, key: &str, label: &str| {
            let color = if on { C_CREATE } else { C_DIM };
            vec![
                Span::styled(
                    format!("{} {label}", if on { "[x]" } else { "[ ]" }),
                    Style::default().fg(color),
                ),
                Span::styled(format!(" ({key})   "), Style::default().fg(C_DIM)),
            ]
        };
        let mut row = vec![Span::raw(PAD)];
        row.extend(chip(overwrite, "^o", t::opt_overwrite()));
        row.extend(chip(render, "^r", t::opt_render()));
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(row));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}

/// The shape every "answer one question" screen shares: the question on the
/// top line, then hint lines each ending in a highlighted example.
fn draw_lines(f: &mut Frame, top: Rect, body: Rect, question: &str, hints: &[(&str, &str)]) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(PAD),
            Span::styled(question.to_string(), Style::default().fg(C_BRANCH)),
        ])),
        top,
    );
    let mut lines = vec![Line::from(Span::raw(""))];
    for (hint, example) in hints {
        lines.push(Line::from(vec![
            Span::raw(PAD),
            Span::styled(hint.to_string(), Style::default().fg(C_DIM)),
            Span::styled(example.to_string(), Style::default().fg(C_CREATE)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}

fn draw_cache_modes(f: &mut Frame, area: Rect, cursor: usize) {
    let desc = |m: CacheMode| match m {
        CacheMode::Keyed => t::cache_mode_keyed_desc(),
        CacheMode::Shared => t::cache_mode_shared_desc(),
        CacheMode::Private => t::cache_mode_private_desc(),
    };
    let lines: Vec<Line> = CacheMode::ALL
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let sel = i == cursor;
            Line::from(vec![
                Span::styled(
                    if sel { POINTER } else { PAD },
                    Style::default().fg(C_POINTER).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    fit(m.as_str(), 9),
                    Style::default().fg(C_TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(desc(*m), Style::default().fg(C_DIM)),
            ])
            .style(if sel {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_cmd_help(f: &mut Frame, top: Rect, body: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(PAD),
            Span::styled(t::cmd_question(), Style::default().fg(C_BRANCH)),
        ])),
        top,
    );
    let lines = vec![
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::raw(PAD),
            Span::styled(t::cmd_hint(), Style::default().fg(C_DIM)),
            Span::styled("npm ci", Style::default().fg(C_CREATE)),
        ]),
        Line::from(vec![
            Span::raw(PAD),
            Span::styled(t::cmd_more_in_toml(), Style::default().fg(C_DIM)),
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
        Mode::ConfirmRemove { step, .. } => Line::from(vec![
            Span::styled(
                " remove ",
                Style::default().fg(C_ERR).add_modifier(Modifier::BOLD),
            ),
            Span::raw(match step.dst() {
                Some(dst) => format!(
                    "'{}' and undo (worktree)/{dst} everywhere ? y/N",
                    step.subject()
                ),
                None => format!("'{}' ? y/N", step.subject()),
            }),
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
            return draw_prompt(f, area, t::label_filter(), &app.filter, true);
        }
        _ => Line::from(vec![
            Span::styled(t::label_recipe(), Style::default().fg(C_DIM)),
            Span::styled(
                trunc_left(
                    &app.layout.sync_config.display().to_string(),
                    area.width.saturating_sub(12) as usize,
                ),
                Style::default().fg(C_PATH),
            ),
        ]),
    };
    f.render_widget(Paragraph::new(line), area);
}
