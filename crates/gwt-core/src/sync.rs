//! The sync recipe: everything a fresh worktree needs that git does not carry.
//!
//! This replaces the old secrets manifest, which could only ever say "symlink
//! this file". A recipe is an ordered list of steps, and the order is the point:
//! you want `.env` in place *before* `direnv allow`, and both before `npm ci`.
//!
//! Two of the three step kinds move a file, and they use different bases — the
//! one thing people got wrong about the old manifest, so it stays spelled out:
//!
//!   src  — relative to the REPO ROOT (`layout.root`, where .git / .bare live)
//!   dst  — relative to EACH WORKTREE ROOT
//!
//!   <repo-root>/
//!   ├── .gwt/sync.toml            <- the recipe (never inside a worktree)
//!   ├── secrets/.env              <- src = "secrets/.env"
//!   ├── default/.env   -> symlink to ../secrets/.env
//!   └── feature-a/.env -> symlink to ../secrets/.env
//!                 ^^^^            <- dst = ".env"
//!
//! The recipe living at the repo root, outside every worktree, is also what
//! makes `run` steps safe to have at all: `git pull` cannot introduce one.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::cache::{self, BindOutcome, CacheMode, CacheStep};
use crate::error::{Error, Result};
use crate::layout::{strip_slash, BareLayout};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// model
// ---------------------------------------------------------------------------

/// When a `run` step is allowed to fire.
///
/// Linking and copying are idempotent, so they always happen. Running a command
/// is not, which is why `create` is the default: `git wt sync apply` repairs
/// links without re-running anyone's `npm ci` unless they asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A worktree was just created (`add` / `new` / `review` / `clone`).
    Create,
    /// `git wt sync apply` — re-applying the recipe to existing worktrees.
    Apply,
    /// Only ever by explicit request.
    Manual,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Create => "create",
            Phase::Apply => "apply",
            Phase::Manual => "manual",
        }
    }

    fn parse(s: &str) -> Option<Phase> {
        match s.trim() {
            "create" => Some(Phase::Create),
            "apply" => Some(Phase::Apply),
            "manual" => Some(Phase::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStep {
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyStep {
    pub src: String,
    pub dst: String,
    /// Overwrite a file already sitting at the destination. Off by default:
    /// a copy is the kind people edit in place, so clobbering it loses work.
    pub overwrite: bool,
    /// Substitute `{{branch}}`, `{{worktree}}`, `{{worktree_name}}`, `{{root}}`.
    pub render: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStep {
    pub cmd: String,
    pub when: Vec<Phase>,
    /// Only run where this path exists inside the worktree. A monorepo has
    /// worktrees that need `npm ci` and worktrees that do not.
    pub only_if: Option<String>,
    pub timeout: Duration,
    /// Working directory, relative to the worktree root. Defaults to its root.
    pub dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Link(LinkStep),
    Copy(CopyStep),
    Run(RunStep),
    /// A build cache mounted from outside the worktree. It belongs in the same
    /// ordered list as the rest: the cache has to be in place before the `run`
    /// step that fills it.
    Cache(CacheStep),
}

impl Step {
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Link(_) => "link",
            Step::Copy(_) => "copy",
            Step::Run(_) => "run",
            Step::Cache(_) => "cache",
        }
    }

    /// The left-hand column: a source path, a command line, or what decides
    /// which worktrees a cache is shared with.
    pub fn subject(&self) -> String {
        match self {
            Step::Link(s) => s.src.clone(),
            Step::Copy(s) => s.src.clone(),
            Step::Run(s) => s.cmd.clone(),
            Step::Cache(c) => c.summary(),
        }
    }

    /// `subject` folded onto one line, for tables and status lines.
    ///
    /// A `run` step's command may be a whole script; every screen that lists
    /// steps has exactly one line to show it in, and a raw newline there does
    /// not truncate — it corrupts the rest of the table.
    pub fn subject_line(&self) -> String {
        one_line(&self.subject())
    }

    /// How many lines the command spans, for the kinds that have one.
    pub fn cmd_lines(&self) -> usize {
        match self {
            Step::Run(r) => r.cmd.lines().filter(|l| !l.trim().is_empty()).count(),
            _ => 0,
        }
    }

    /// The right-hand column: where it lands inside a worktree.
    pub fn dst(&self) -> Option<&str> {
        match self {
            Step::Link(s) => Some(&s.dst),
            Step::Copy(s) => Some(&s.dst),
            Step::Run(_) => None,
            Step::Cache(c) => Some(&c.path),
        }
    }

    /// Source path, for the kinds that read one out of the repo root.
    pub fn src(&self) -> Option<&str> {
        match self {
            Step::Link(s) => Some(&s.src),
            Step::Copy(s) => Some(&s.src),
            Step::Run(_) | Step::Cache(_) => None,
        }
    }

    pub fn src_abs(&self, layout: &BareLayout) -> Option<PathBuf> {
        self.src().map(|s| layout.root.join(s))
    }

    pub fn dst_abs(&self, worktree_dir: &Path) -> Option<PathBuf> {
        self.dst().map(|d| worktree_dir.join(d))
    }

    fn runs_in(&self, phase: Phase) -> bool {
        match self {
            // A file that should be there is a file that should be there,
            // whichever way we got here. Re-binding a cache is likewise
            // idempotent, and re-checking the key is the point of doing it.
            Step::Link(_) | Step::Copy(_) | Step::Cache(_) => true,
            Step::Run(r) => r.when.contains(&phase),
        }
    }
}

/// The first non-blank line of `s`, marked with `…` when more follow.
pub fn one_line(s: &str) -> String {
    let mut lines = s
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("").to_string();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

/// How the recipe reached us — the CLI says so once, because a user editing
/// `secrets/manifest` while gwt reads `.gwt/sync.toml` would be baffling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `.gwt/sync.toml`
    Toml,
    /// The pre-0.7 `secrets/manifest`, read for compatibility.
    Legacy,
    /// Neither file exists yet.
    Missing,
}

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub steps: Vec<Step>,
    pub origin: Origin,
}

// ---------------------------------------------------------------------------
// path normalization
// ---------------------------------------------------------------------------

/// Normalize a user-supplied source path into a repo-root-relative path.
///
/// Accepts an absolute path inside the root, or one relative to `cwd` — which
/// is what the shell just tab-completed against, and no longer necessarily the
/// root now that these subcommands run from inside a worktree too. From the
/// root the two readings coincide, so `sync add secrets/.env` is unchanged.
pub fn normalize_src(layout: &BareLayout, cwd: &Path, input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::SyncSrcInvalid {
            path: input.to_string(),
            reason: "path is empty",
        });
    }
    let p = Path::new(raw);
    let absolute = p.is_absolute();
    // Compare against the canonical root so /var vs /private/var (macOS) and
    // symlinked checkouts don't spuriously look "outside".
    let root = fs::canonicalize(&layout.root).unwrap_or_else(|_| layout.root.clone());
    let cand = canonicalize_lexically_existing(&if absolute {
        p.to_path_buf()
    } else {
        cwd.join(p)
    });
    let rel = cand
        .strip_prefix(&root)
        .map_err(|_| Error::SyncSrcInvalid {
            path: input.to_string(),
            reason: if absolute {
                "absolute path is outside the repo root"
            } else {
                "path is outside the repo root"
            },
        })?
        .to_path_buf();

    lexical_normalize(&rel).ok_or_else(|| Error::SyncSrcInvalid {
        path: input.to_string(),
        reason: "path escapes the repo root",
    })
}

/// Normalize a user-supplied destination into a worktree-relative path.
///
/// The destination is always relative to a worktree root, so an absolute path is
/// a category error rather than something to reinterpret — reject it loudly.
pub fn normalize_dst(input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::SyncDstInvalid {
            path: input.to_string(),
            reason: "path is empty",
        });
    }
    if Path::new(raw).is_absolute() {
        return Err(Error::SyncDstInvalid {
            path: input.to_string(),
            reason: "must be relative to the worktree root, not absolute",
        });
    }
    lexical_normalize(Path::new(raw)).ok_or_else(|| Error::SyncDstInvalid {
        path: input.to_string(),
        reason: "path escapes the worktree root",
    })
}

/// Collapse `.` / `..` textually and re-emit with `/` separators.
/// Returns `None` if the path escapes its base or resolves to nothing.
fn lexical_normalize(p: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Canonicalize as much of `p` as exists, keeping the non-existent tail. Plain
/// `fs::canonicalize` fails outright when the file isn't created yet, but we want
/// to register steps for files that will appear later.
fn canonicalize_lexically_existing(p: &Path) -> PathBuf {
    if let Ok(c) = fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => canonicalize_lexically_existing(parent).join(name),
        _ => p.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// reading
// ---------------------------------------------------------------------------

pub fn load(layout: &BareLayout) -> Result<SyncConfig> {
    if layout.sync_config.exists() {
        let raw = fs::read_to_string(&layout.sync_config)?;
        return Ok(SyncConfig {
            steps: parse_toml(&raw)?,
            origin: Origin::Toml,
        });
    }
    if layout.legacy_manifest.exists() {
        let raw = fs::read_to_string(&layout.legacy_manifest)?;
        return Ok(SyncConfig {
            steps: parse_legacy(&raw),
            origin: Origin::Legacy,
        });
    }
    Ok(SyncConfig {
        steps: Vec::new(),
        origin: Origin::Missing,
    })
}

/// Every `[[step]]` in the document, in file order.
///
/// Tables other than `step` are ignored rather than rejected: `[[cache]]` is
/// parsed by its own module, and an unknown key is far more likely to be a
/// newer gwt's than a typo worth refusing to start over.
pub fn parse_toml(raw: &str) -> Result<Vec<Step>> {
    let doc: toml_edit::DocumentMut =
        raw.parse()
            .map_err(|e: toml_edit::TomlError| Error::SyncConfigInvalid {
                reason: e.to_string(),
            })?;
    let Some(array) = doc.get("step").and_then(|i| i.as_array_of_tables()) else {
        return Ok(Vec::new());
    };
    array
        .iter()
        .enumerate()
        .map(|(i, t)| step_from_table(i, t))
        .collect()
}

fn step_from_table(idx: usize, t: &toml_edit::Table) -> Result<Step> {
    let at = |what: &str| Error::SyncConfigInvalid {
        reason: format!("[[step]] #{}: {what}", idx + 1),
    };
    let kind = t
        .get("type")
        .and_then(|i| i.as_str())
        .ok_or_else(|| at("missing `type` (one of link, copy, run)"))?;

    let string = |key: &str| t.get(key).and_then(|i| i.as_str()).map(str::to_string);
    let flag = |key: &str, default: bool| t.get(key).and_then(|i| i.as_bool()).unwrap_or(default);

    match kind {
        "link" | "copy" => {
            let src = string("src").ok_or_else(|| at("`src` is required"))?;
            let dst = string("dst").ok_or_else(|| at("`dst` is required"))?;
            let src = lexical_normalize(Path::new(strip_slash(src.trim())))
                .ok_or_else(|| at("`src` escapes the repo root"))?;
            let dst = lexical_normalize(Path::new(strip_slash(dst.trim())))
                .ok_or_else(|| at("`dst` escapes the worktree root"))?;
            Ok(if kind == "link" {
                Step::Link(LinkStep { src, dst })
            } else {
                Step::Copy(CopyStep {
                    src,
                    dst,
                    overwrite: flag("overwrite", false),
                    render: flag("render", false),
                })
            })
        }
        "run" => {
            let cmd = string("cmd").ok_or_else(|| at("`cmd` is required"))?;
            let when =
                match t.get("when").and_then(|i| i.as_array()) {
                    None => vec![Phase::Create],
                    Some(arr) => {
                        let mut phases = Vec::new();
                        for v in arr.iter() {
                            let s = v.as_str().ok_or_else(|| at("`when` must hold strings"))?;
                            phases.push(Phase::parse(s).ok_or_else(|| {
                                at("`when` accepts only create, apply and manual")
                            })?);
                        }
                        phases
                    }
                };
            let timeout = match string("timeout") {
                None => DEFAULT_TIMEOUT,
                Some(s) => {
                    parse_timeout(&s).ok_or_else(|| at("`timeout` looks like 30s, 10m or 1h"))?
                }
            };
            Ok(Step::Run(RunStep {
                cmd,
                when,
                only_if: string("only_if"),
                timeout,
                dir: string("dir"),
            }))
        }
        "cache" => {
            let path = string("path").ok_or_else(|| at("`path` is required"))?;
            let path = lexical_normalize(Path::new(strip_slash(path.trim())))
                .ok_or_else(|| at("`path` escapes the worktree root"))?;
            let mode = match string("mode") {
                None => CacheMode::Keyed,
                Some(m) => CacheMode::parse(&m)
                    .ok_or_else(|| at("`mode` accepts only keyed, shared and private"))?,
            };
            let mut key = Vec::new();
            if let Some(arr) = t.get("key").and_then(|i| i.as_array()) {
                for v in arr.iter() {
                    key.push(
                        v.as_str()
                            .ok_or_else(|| at("`key` must hold file paths"))?
                            .to_string(),
                    );
                }
            }
            Ok(Step::Cache(CacheStep {
                path,
                mode,
                key,
                seed: flag("seed", true),
                env: string("env"),
            }))
        }
        other => Err(at(&format!(
            "unknown type '{other}' (expected link, copy, run or cache)"
        ))),
    }
}

/// `30s`, `10m`, `1h`, or a bare number of seconds.
pub fn parse_timeout(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().ok()?;
    Some(Duration::from_secs(n.checked_mul(mult)?))
}

fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s % 3600 == 0 && s > 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 && s > 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// The pre-0.7 manifest: one `src<TAB>dst` symlink mapping per line.
pub fn parse_legacy(raw: &str) -> Vec<Step> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            // We always wrote a tab, so prefer it — that keeps paths containing
            // spaces intact. Fall back to any whitespace for manifests written
            // by the original bash version (or edited by hand).
            let (src, dst) = match trimmed.split_once('\t') {
                Some(pair) => pair,
                None => trimmed.split_once(char::is_whitespace)?,
            };
            // Leading slashes are legacy noise from the bash version — both
            // columns were always relative, so drop them rather than reject.
            let src = lexical_normalize(Path::new(strip_slash(src.trim())))?;
            let dst = lexical_normalize(Path::new(strip_slash(dst.trim())))?;
            Some(Step::Link(LinkStep { src, dst }))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// writing
// ---------------------------------------------------------------------------

/// Write the recipe back, keeping whatever else the file holds.
///
/// The existing document is edited in place rather than regenerated, so
/// comments, key order and the `[[cache]]` tables written by `git wt cache`
/// all survive an edit made from the TUI.
pub fn save(layout: &BareLayout, steps: &[Step]) -> Result<()> {
    let mut doc: toml_edit::DocumentMut = match fs::read_to_string(&layout.sync_config) {
        Ok(raw) => raw
            .parse()
            .map_err(|e: toml_edit::TomlError| Error::SyncConfigInvalid {
                reason: e.to_string(),
            })?,
        Err(_) => new_document(),
    };

    if !doc.contains_key("step") {
        doc["step"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let array = doc["step"]
        .as_array_of_tables_mut()
        .ok_or_else(|| Error::SyncConfigInvalid {
            reason: "`step` is not a list of [[step]] tables".into(),
        })?;

    while array.len() > steps.len() {
        array.remove(array.len() - 1);
    }
    for (i, step) in steps.iter().enumerate() {
        match array.get_mut(i) {
            // Reuse the table already there so its comments stay attached.
            Some(t) => write_step(t, step),
            None => {
                let mut t = toml_edit::Table::new();
                write_step(&mut t, step);
                array.push(t);
            }
        }
    }

    if let Some(parent) = layout.sync_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&layout.sync_config, doc.to_string())?;
    Ok(())
}

/// Drop an empty, commented recipe into a fresh clone. A file that exists and
/// explains itself is findable; a feature nobody knows the file name of is not.
pub fn write_starter(layout: &BareLayout) -> Result<()> {
    if layout.sync_config.exists() {
        return Ok(());
    }
    fs::create_dir_all(&layout.gwt_dir)?;
    fs::write(&layout.sync_config, new_document().to_string())?;
    Ok(())
}

fn new_document() -> toml_edit::DocumentMut {
    let mut doc = toml_edit::DocumentMut::new();
    doc["version"] = toml_edit::value(1);
    doc.decor_mut().set_prefix(
        "# git wt sync — what every worktree needs that git does not carry.\n\
         # src is relative to this directory's parent (the repo root),\n\
         # dst is relative to each worktree's own root.\n\n",
    );
    doc
}

/// A command as TOML.
///
/// A multi-line command is a shell script, and a script written as
/// `"set -e\nnpm ci\nnpm run build"` is unreadable in the very file people are
/// meant to be able to open and edit. Emit those as a `'''` block instead, and
/// only when it round-trips exactly — no quoting rule is worth a recipe that
/// reads back as something else.
fn cmd_value(cmd: &str) -> toml_edit::Item {
    if cmd.contains('\n') {
        let doc: std::result::Result<toml_edit::DocumentMut, _> =
            format!("cmd = '''\n{cmd}'''").parse();
        if let Ok(doc) = doc {
            if doc["cmd"].as_str() == Some(cmd) {
                return doc["cmd"].clone();
            }
        }
    }
    toml_edit::value(cmd)
}

fn write_step(t: &mut toml_edit::Table, step: &Step) {
    // Any key the new kind does not use has to go, or a link that became a run
    // would keep a stale `dst` around.
    let keep: &[&str] = match step {
        Step::Link(_) => &["type", "src", "dst"],
        Step::Copy(_) => &["type", "src", "dst", "overwrite", "render"],
        Step::Run(_) => &["type", "cmd", "when", "only_if", "timeout", "dir"],
        Step::Cache(_) => &["type", "path", "mode", "key", "seed", "env"],
    };
    let stale: Vec<String> = t
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !keep.contains(&k.as_str()))
        .collect();
    for k in stale {
        t.remove(&k);
    }

    t["type"] = toml_edit::value(step.kind());
    match step {
        Step::Link(s) => {
            t["src"] = toml_edit::value(&s.src);
            t["dst"] = toml_edit::value(&s.dst);
        }
        Step::Copy(s) => {
            t["src"] = toml_edit::value(&s.src);
            t["dst"] = toml_edit::value(&s.dst);
            t["overwrite"] = toml_edit::value(s.overwrite);
            t["render"] = toml_edit::value(s.render);
        }
        Step::Run(s) => {
            t["cmd"] = cmd_value(&s.cmd);
            let mut when = toml_edit::Array::new();
            for p in &s.when {
                when.push(p.as_str());
            }
            t["when"] = toml_edit::value(when);
            match &s.only_if {
                Some(v) => t["only_if"] = toml_edit::value(v),
                None => {
                    t.remove("only_if");
                }
            }
            t["timeout"] = toml_edit::value(format_duration(s.timeout));
            match &s.dir {
                Some(v) => t["dir"] = toml_edit::value(v),
                None => {
                    t.remove("dir");
                }
            }
        }
        Step::Cache(c) => {
            t["path"] = toml_edit::value(&c.path);
            t["mode"] = toml_edit::value(c.mode.as_str());
            if c.mode == CacheMode::Keyed {
                let mut key = toml_edit::Array::new();
                for k in &c.key {
                    key.push(k.as_str());
                }
                t["key"] = toml_edit::value(key);
            } else {
                // A key that cannot affect anything is a question waiting to be
                // asked; leave the file saying only what it does.
                t.remove("key");
            }
            t["seed"] = toml_edit::value(c.seed);
            match &c.env {
                Some(v) => t["env"] = toml_edit::value(v),
                None => {
                    t.remove("env");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// applying
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Linked,
    Copied,
    Ran {
        code: i32,
        secs: u64,
    },
    /// A cache is now mounted from its bucket.
    Mounted {
        bucket: String,
        adopted: bool,
        seeded: bool,
    },
    /// The cache could not be mounted without mixing two caches together.
    Blocked {
        reason: &'static str,
    },
    Skipped {
        reason: &'static str,
    },
    Failed {
        detail: String,
    },
}

impl Outcome {
    /// Whether the user has to do something about it. `Blocked` counts: the
    /// cache is not mounted, and only a human can decide which copy to keep.
    pub fn is_failure(&self) -> bool {
        match self {
            Outcome::Failed { .. } | Outcome::Blocked { .. } => true,
            Outcome::Ran { code, .. } => *code != 0,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlinkOutcome {
    /// The symlink we manage was removed.
    Removed,
    /// Nothing was there — already clean.
    Absent,
    /// Something else occupies the path; we refuse to delete it.
    Kept { reason: &'static str },
}

/// What the caller learns while a recipe runs. `Output` arrives line by line so
/// a five-minute `npm ci` shows something other than a frozen terminal.
pub enum Event<'a> {
    StepStart(&'a Step),
    Output(&'a str),
    StepDone(&'a Step, &'a Outcome),
}

pub type Reporter<'a> = &'a mut dyn FnMut(Event);

/// A reporter for callers with nothing to show: `&mut sync::noop`.
pub fn noop(_: Event) {}

/// Apply every step of the recipe to one worktree.
pub fn apply(
    layout: &BareLayout,
    worktree_dir: &Path,
    phase: Phase,
    report: Reporter,
) -> Result<Vec<(Step, Outcome)>> {
    let cfg = load(layout)?;
    apply_steps(layout, worktree_dir, &cfg.steps, phase, report)
}

/// Apply the recipe with no interest in the running commentary.
pub fn apply_quiet(
    layout: &BareLayout,
    worktree_dir: &Path,
    phase: Phase,
) -> Result<Vec<(Step, Outcome)>> {
    apply(layout, worktree_dir, phase, &mut noop)
}

pub fn apply_steps(
    layout: &BareLayout,
    worktree_dir: &Path,
    steps: &[Step],
    phase: Phase,
    report: Reporter,
) -> Result<Vec<(Step, Outcome)>> {
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        if !step.runs_in(phase) {
            out.push((
                step.clone(),
                Outcome::Skipped {
                    reason: "not in this phase",
                },
            ));
            continue;
        }
        report(Event::StepStart(step));
        let outcome = apply_step(layout, worktree_dir, step, report)?;
        report(Event::StepDone(step, &outcome));
        out.push((step.clone(), outcome));
    }
    Ok(out)
}

pub fn apply_step(
    layout: &BareLayout,
    worktree_dir: &Path,
    step: &Step,
    report: Reporter,
) -> Result<Outcome> {
    match step {
        Step::Link(s) => apply_link(layout, worktree_dir, s),
        Step::Copy(s) => apply_copy(layout, worktree_dir, s),
        Step::Run(s) => apply_run(layout, worktree_dir, s, report),
        Step::Cache(c) => Ok(match cache::bind(layout, worktree_dir, c)? {
            BindOutcome::Bound {
                bucket,
                adopted,
                seeded,
            } => Outcome::Mounted {
                bucket,
                adopted,
                seeded,
            },
            BindOutcome::Unchanged { bucket } => Outcome::Mounted {
                bucket,
                adopted: false,
                seeded: false,
            },
            BindOutcome::Blocked { reason } => Outcome::Blocked { reason },
        }),
    }
}

fn apply_link(layout: &BareLayout, worktree_dir: &Path, s: &LinkStep) -> Result<Outcome> {
    let src_abs = layout.root.join(&s.src);
    let dst_abs = worktree_dir.join(&s.dst);
    if !src_abs.exists() {
        return Ok(Outcome::Skipped {
            reason: "source missing",
        });
    }
    if let Some(parent) = dst_abs.parent() {
        fs::create_dir_all(parent)?;
    }
    // Replace any existing file/symlink at the destination, mirroring `ln -sf`.
    if dst_abs.symlink_metadata().is_ok() {
        let _ = fs::remove_file(&dst_abs);
    }
    symlink(&src_abs, &dst_abs)?;
    Ok(Outcome::Linked)
}

fn apply_copy(layout: &BareLayout, worktree_dir: &Path, s: &CopyStep) -> Result<Outcome> {
    let src_abs = layout.root.join(&s.src);
    let dst_abs = worktree_dir.join(&s.dst);
    if !src_abs.exists() {
        return Ok(Outcome::Skipped {
            reason: "source missing",
        });
    }
    // A copy is the kind of file people edit in place. Refusing to overwrite is
    // the difference between a recipe you can re-run and one that eats work.
    if dst_abs.symlink_metadata().is_ok() && !s.overwrite {
        return Ok(Outcome::Skipped {
            reason: "destination exists",
        });
    }
    if let Some(parent) = dst_abs.parent() {
        fs::create_dir_all(parent)?;
    }
    if s.render {
        let raw = fs::read_to_string(&src_abs)?;
        fs::write(&dst_abs, render_template(&raw, layout, worktree_dir))?;
    } else {
        if dst_abs.symlink_metadata().is_ok() {
            let _ = fs::remove_file(&dst_abs);
        }
        fs::copy(&src_abs, &dst_abs)?;
    }
    Ok(Outcome::Copied)
}

fn worktree_name(worktree_dir: &Path) -> String {
    worktree_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn branch_of(worktree_dir: &Path) -> String {
    crate::git::run(worktree_dir, ["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn render_template(raw: &str, layout: &BareLayout, worktree_dir: &Path) -> String {
    raw.replace("{{branch}}", &branch_of(worktree_dir))
        .replace("{{worktree}}", &worktree_dir.display().to_string())
        .replace("{{worktree_name}}", &worktree_name(worktree_dir))
        .replace("{{root}}", &layout.root.display().to_string())
}

fn apply_run(
    layout: &BareLayout,
    worktree_dir: &Path,
    s: &RunStep,
    report: Reporter,
) -> Result<Outcome> {
    if let Some(cond) = &s.only_if {
        if !worktree_dir.join(cond).exists() {
            return Ok(Outcome::Skipped {
                reason: "condition unmet",
            });
        }
    }
    let dir = match &s.dir {
        Some(d) => worktree_dir.join(d),
        None => worktree_dir.to_path_buf(),
    };
    if !dir.is_dir() {
        return Ok(Outcome::Failed {
            detail: format!("dir '{}' does not exist", dir.display()),
        });
    }

    let mut cmd = shell_command(&s.cmd);
    cmd.current_dir(&dir)
        .env("GWT_ROOT", &layout.root)
        .env("GWT_WORKTREE", worktree_dir)
        .env("GWT_WORKTREE_NAME", worktree_name(worktree_dir))
        .env("GWT_BRANCH", branch_of(worktree_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(Outcome::Failed {
                detail: e.to_string(),
            })
        }
    };

    // Both pipes have to be drained while we wait, or a chatty command fills its
    // buffer and blocks forever — which would look exactly like a hung build.
    let (tx, rx) = mpsc::channel::<String>();
    for stream in [
        child.stdout.take().map(Pipe::Out),
        child.stderr.take().map(Pipe::Err),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader: Box<dyn std::io::Read + Send> = match stream {
                Pipe::Out(s) => Box::new(s),
                Pipe::Err(s) => Box::new(s),
            };
            for line in BufReader::new(reader)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let deadline = started + s.timeout;
    let mut timed_out = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(line) => report(Event::Output(&line)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Either the deadline passed or the command is simply quiet;
                // `try_wait` tells the two apart.
                if child.try_wait()?.is_some() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if timed_out {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(Outcome::Failed {
            detail: format!("timed out after {}", format_duration(s.timeout)),
        });
    }
    let status = child.wait()?;
    // Anything still queued when the pipes closed is part of the output.
    while let Ok(line) = rx.try_recv() {
        report(Event::Output(&line));
    }
    Ok(Outcome::Ran {
        code: status.code().unwrap_or(-1),
        secs: started.elapsed().as_secs(),
    })
}

enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

#[cfg(unix)]
fn shell_command(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

#[cfg(windows)]
fn shell_command(cmd: &str) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(cmd);
    c
}

/// Undo one step in one worktree.
///
/// Only ever removes a symlink that still points at this step's source, or a
/// copy the user has not touched — `sync rm` must never be able to destroy
/// work. A `run` step has nothing to undo.
pub fn unlink_step(layout: &BareLayout, worktree_dir: &Path, step: &Step) -> Result<UnlinkOutcome> {
    // Unmounting a cache leaves the bucket alone: the data is the expensive
    // part, and `git wt cache gc` is where deleting it is an explicit act.
    if let Step::Cache(c) = step {
        return Ok(if cache::unbind(layout, worktree_dir, c)? {
            UnlinkOutcome::Removed
        } else {
            UnlinkOutcome::Absent
        });
    }
    let (Some(dst), Some(src_abs)) = (step.dst(), step.src_abs(layout)) else {
        return Ok(UnlinkOutcome::Kept {
            reason: "nothing to remove",
        });
    };
    let dst_abs = worktree_dir.join(dst);
    let Ok(meta) = dst_abs.symlink_metadata() else {
        return Ok(UnlinkOutcome::Absent);
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(&dst_abs)?;
        if target != src_abs {
            return Ok(UnlinkOutcome::Kept {
                reason: "symlink points elsewhere",
            });
        }
        fs::remove_file(&dst_abs)?;
        return Ok(UnlinkOutcome::Removed);
    }
    // A copy leaves a real file behind on purpose, so removing it is only safe
    // while it is still byte-identical to what we put there. Say which of the
    // three reasons applies — "not a symlink" was true and useless.
    match step {
        Step::Copy(c) if c.render => Ok(UnlinkOutcome::Kept {
            reason: "a rendered copy, which may have been edited",
        }),
        Step::Copy(_) if same_contents(&src_abs, &dst_abs) => {
            fs::remove_file(&dst_abs)?;
            Ok(UnlinkOutcome::Removed)
        }
        Step::Copy(_) => Ok(UnlinkOutcome::Kept {
            reason: "the copy differs from its source",
        }),
        _ => Ok(UnlinkOutcome::Kept {
            reason: "a real file, not the link we made",
        }),
    }
}

fn same_contents(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Whether `worktree_dir` currently carries what this step promises.
pub fn is_applied(layout: &BareLayout, worktree_dir: &Path, step: &Step) -> bool {
    match step {
        Step::Link(s) => {
            let dst = worktree_dir.join(&s.dst);
            dst.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
                && fs::read_link(&dst).is_ok_and(|t| t == layout.root.join(&s.src))
        }
        Step::Copy(s) => worktree_dir.join(&s.dst).exists(),
        // A command leaves no mark we can honestly check for.
        Step::Run(_) => false,
        Step::Cache(c) => cache::is_bound(layout, worktree_dir, c),
    }
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    // Files are the dominant case for the recipe; if the user pointed at a
    // directory, fall back to a directory symlink (needs Dev Mode / admin).
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_kinds() {
        let raw = r#"
version = 1

[[step]]
type = "link"
src = "secrets/.env"
dst = ".env"

[[step]]
type = "copy"
src = "secrets/env.sample"
dst = "config/.env"
render = true

[[step]]
type = "run"
cmd = "npm ci"
only_if = "package.json"
timeout = "3m"
"#;
        let steps = parse_toml(raw).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(
            steps[0],
            Step::Link(LinkStep {
                src: "secrets/.env".into(),
                dst: ".env".into()
            })
        );
        let Step::Copy(c) = &steps[1] else {
            panic!("expected a copy")
        };
        assert!(c.render && !c.overwrite);
        let Step::Run(r) = &steps[2] else {
            panic!("expected a run")
        };
        assert_eq!(r.timeout, Duration::from_secs(180));
        assert_eq!(r.when, vec![Phase::Create]);
        assert_eq!(r.only_if.as_deref(), Some("package.json"));
    }

    #[test]
    fn a_bad_step_names_itself() {
        let e = parse_toml("[[step]]\ntype = \"link\"\nsrc = \"a\"\n").unwrap_err();
        assert!(e.to_string().contains("#1"), "{e}");
        assert!(e.to_string().contains("dst"), "{e}");
    }

    #[test]
    fn unknown_tables_are_left_alone() {
        let raw = "[[cache]]\npath = \"target\"\n";
        assert!(parse_toml(raw).unwrap().is_empty());
    }

    #[test]
    fn legacy_manifest_becomes_link_steps() {
        let raw = "# comment\nfoo\tbar\n\nbaz   qux\n/leading\t/slash\n";
        let v = parse_legacy(raw);
        assert_eq!(v.len(), 3);
        assert_eq!(
            v[0],
            Step::Link(LinkStep {
                src: "foo".into(),
                dst: "bar".into()
            })
        );
        assert_eq!(
            v[2],
            Step::Link(LinkStep {
                src: "leading".into(),
                dst: "slash".into()
            })
        );
    }

    #[test]
    fn tab_separated_legacy_paths_may_contain_spaces() {
        let v = parse_legacy("secrets/my env\tconfig/my env\n");
        assert_eq!(
            v[0],
            Step::Link(LinkStep {
                src: "secrets/my env".into(),
                dst: "config/my env".into()
            })
        );
    }

    #[test]
    fn normalizes_dot_segments() {
        assert_eq!(
            lexical_normalize(Path::new("./secrets/../secrets/.env")).unwrap(),
            "secrets/.env"
        );
    }

    #[test]
    fn rejects_escaping_and_absolute_destinations() {
        assert!(normalize_dst("../outside").is_err());
        assert!(normalize_dst("/etc/passwd").is_err());
        assert!(normalize_dst("").is_err());
        assert_eq!(normalize_dst("./config/.env").unwrap(), "config/.env");
    }

    #[test]
    fn a_script_is_written_as_a_readable_block() {
        let cmd = "set -e\ncd api\nnpm ci";
        let item = cmd_value(cmd);
        let rendered = format!("cmd = {}", item.as_value().unwrap());
        assert!(
            rendered.contains("'''"),
            "a script should be a TOML block, got {rendered:?}"
        );
        // What matters is the value that comes back, not how it looks.
        let doc: toml_edit::DocumentMut = rendered.parse().unwrap();
        assert_eq!(doc["cmd"].as_str(), Some(cmd));
    }

    #[test]
    fn a_script_survives_a_full_save_and_reload() {
        let cmd = "set -e\n# it's quoted, ''' and all\nnpm ci";
        let step = Step::Run(RunStep {
            cmd: cmd.into(),
            when: vec![Phase::Create],
            only_if: None,
            timeout: DEFAULT_TIMEOUT,
            dir: Some("api".into()),
        });
        let mut table = toml_edit::Table::new();
        write_step(&mut table, &step);
        let raw = format!("[[step]]\n{table}");
        let back = parse_toml(&raw).unwrap();
        assert_eq!(back, vec![step]);
    }

    #[test]
    fn a_multi_line_command_shows_as_one_line() {
        let step = Step::Run(RunStep {
            cmd: "set -e\n\nnpm ci\nnpm run build".into(),
            when: vec![Phase::Create],
            only_if: None,
            timeout: DEFAULT_TIMEOUT,
            dir: None,
        });
        assert_eq!(step.subject_line(), "set -e …");
        assert_eq!(step.cmd_lines(), 3);
        assert_eq!(one_line("npm ci"), "npm ci");
    }

    #[test]
    fn durations_round_trip() {
        assert_eq!(parse_timeout("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_timeout("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_timeout("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_timeout("45"), Some(Duration::from_secs(45)));
        assert_eq!(parse_timeout("soon"), None);
        assert_eq!(format_duration(Duration::from_secs(600)), "10m");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
    }
}
