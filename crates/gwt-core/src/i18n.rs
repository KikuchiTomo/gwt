//! Language selection and the message catalog.
//!
//! Resolution order, most specific first:
//!   1. `--lang <code>` on the command line
//!   2. `$GWT_LANG`
//!   3. `~/.config/gwt/config`  (written by `git wt config lang …`)
//!   4. `$LC_ALL` / `$LC_MESSAGES` / `$LANG`
//!   5. English
//!
//! The resolved language is process-global. A CLI has exactly one locale for
//! its whole run, and threading a `&Strings` through every render function
//! would add noise to call sites without buying anything.

use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Ja,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        // Accept bare codes and full locales alike: "ja", "ja_JP.UTF-8", "japanese".
        let s = s.trim().to_ascii_lowercase();
        let head = s.split(['_', '-', '.']).next().unwrap_or("");
        match head {
            "ja" | "jp" | "japanese" => Some(Lang::Ja),
            "en" | "c" | "posix" | "english" => Some(Lang::En),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ja => "日本語",
        }
    }

    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ja];
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// Fix the language for this process. The first call wins.
pub fn set(l: Lang) {
    let _ = LANG.set(l);
}

pub fn current() -> Lang {
    *LANG.get_or_init(|| detect(None))
}

/// Where the language preference is persisted.
pub fn config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("gwt").join("config"))
}

/// Minimal `key = value` reader — a config with one key does not deserve a
/// TOML dependency.
fn config_get(key: &str) -> Option<String> {
    let path = config_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once('=')?;
        if k.trim() == key {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

pub fn read_config_lang() -> Option<Lang> {
    Lang::parse(&config_get("lang")?)
}

/// Persist the preference, preserving any other keys already in the file.
pub fn write_config_lang(l: Lang) -> std::io::Result<PathBuf> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no HOME to store config in")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut kept: Vec<String> = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        for line in raw.lines() {
            let is_lang = line
                .split_once('=')
                .map(|(k, _)| k.trim() == "lang")
                .unwrap_or(false);
            if !is_lang {
                kept.push(line.to_string());
            }
        }
    }
    kept.push(format!("lang = {}", l.code()));
    std::fs::write(&path, format!("{}\n", kept.join("\n")))?;
    Ok(path)
}

fn env_lang(key: &str) -> Option<Lang> {
    Lang::parse(&std::env::var(key).ok()?)
}

pub fn detect(explicit: Option<&str>) -> Lang {
    if let Some(l) = explicit.and_then(Lang::parse) {
        return l;
    }
    if let Some(l) = env_lang("GWT_LANG") {
        return l;
    }
    if let Some(l) = read_config_lang() {
        return l;
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(l) = env_lang(key) {
            return l;
        }
    }
    Lang::En
}

/// Declare a no-argument message in every supported language.
macro_rules! msg {
    ($group:ident { $($(#[$m:meta])* $name:ident => $en:expr, $ja:expr;)* }) => {
        $(
            $(#[$m])*
            pub fn $name() -> &'static str {
                match $crate::i18n::current() {
                    $crate::i18n::Lang::En => $en,
                    $crate::i18n::Lang::Ja => $ja,
                }
            }
        )*
        /// Every entry in this group as `(name, en, ja)`, so tests can check
        /// properties that hold across the whole catalog.
        pub const $group: &[(&str, &str, &str)] =
            &[$((stringify!($name), $en, $ja)),*];
    };
}

/// Pick between two language variants for a value built at runtime.
pub fn pick<T>(en: T, ja: T) -> T {
    match current() {
        Lang::En => en,
        Lang::Ja => ja,
    }
}

pub mod t {
    //! The catalog. Keep entries grouped by the screen they belong to.

    // ---- shared -----------------------------------------------------------
    msg! { SHARED {
        cancel => "cancel", "キャンセル";
        cancel_detail => "leave everything as it is", "何も変更しない";
        press_any_key => " press any key ", " 任意のキーで戻る ";
        working => " working… ", " 実行中… ";
        destructive => "  ⚠ destructive", "  ⚠ 破壊的";
        cannot_be_undone => "this cannot be undone", "この操作は取り消せません";
        confirm_yn => " y: yes, do it   any other key: cancel ",
                      " y: 実行   その他のキー: 中止 ";
        choose_hint => "› press the key in brackets, or ↑↓ + enter",
                       "› [ ] 内のキー、または ↑↓ + Enter";
        help_hint => "  ?:help ", "  ?:ヘルプ ";
    }}

    // ---- worktree picker --------------------------------------------------
    msg! { PICKER {
        picker_help => " ↑↓:nav  enter:cd  p/P:pull/push  d:del  n:new  r:review  f:filter  ?:keys  q:quit ",
                       " ↑↓:移動  enter:移動  p/P:pull/push  d:削除  n:新規  r:レビュー  f:絞込  ?:キー  q:終了 ";
        picker_help_filter => " type:filter  esc:exit  ↑↓/^p^n/^j^k:nav  tab:select  enter:cd ",
                              " 入力:絞込  esc:解除  ↑↓/^p^n/^j^k:移動  tab:選択  enter:移動する ";
        picker_help_selected => " tab/space:select  a:all  d:del  D:force-del  esc:clear  enter:cd ",
                                " tab/space:選択  a:全選択  d:削除  D:強制削除  esc:解除  enter:移動 ";
        filter_prompt_hint => " press f or / to filter ", " f または / で絞り込み ";
        conflict_help => " [key]:choose  ↑↓:move  enter:pick  esc:cancel ",
                         " [キー]:選択  ↑↓:移動  enter:決定  esc:中止 ";
        push_confirm_help => " y: push to remote   any other key: cancel ",
                             " y: リモートへ push   その他のキー: 中止 ";
        pulling => " pulling… ", " pull 中… ";
        pushing => " pushing… ", " push 中… ";
        deleting => " deleting… ", " 削除中… ";
        creating => "creating", "作成中";
        branch_help_new => " type:filter  ↑↓/^p^n:nav  enter:choose base → name  esc:back ",
                           " 入力:絞込  ↑↓/^p^n:移動  enter:基点を選ぶ → 名前  esc:戻る ";
        branch_help_new_dir => " type:filter  ↑↓/^p^n:nav  enter:choose base → name → dir  esc:back ",
                               " 入力:絞込  ↑↓/^p^n:移動  enter:基点 → ブランチ名 → ディレクトリ名  esc:戻る ";
        branch_help_review => " type:filter  ↑↓/^p^n:nav  enter:create wt  esc:back ",
                              " 入力:絞込  ↑↓/^p^n:移動  enter:ワークツリー作成  esc:戻る ";
        name_help => " type:name  enter:create worktree  esc:cancel ",
                     " 名前を入力  enter:ワークツリー作成  esc:中止 ";
        name_help_branch => " type:branch name  enter:next (dir)  esc:cancel ",
                            " ブランチ名を入力  enter:次へ(ディレクトリ)  esc:中止 ";
        name_help_dir => " type:dir name  enter:create worktree  esc:cancel ",
                         " ディレクトリ名を入力  enter:ワークツリー作成  esc:中止 ";
        branching_from => "branching from ", "基点: ";
        name_is_dir_hint => "  → new branch name will also be the worktree dir name",
                            "  → ブランチ名がそのままディレクトリ名になります";
        name_two_step_hint => "  → enter branch name, then worktree dir name",
                              "  → ブランチ名を入力後、ディレクトリ名を入力";
        title_worktree_exists => "worktree already exists", "ワークツリーが既に存在します";
        title_branch_exists => "branch already exists", "ブランチが既に存在します";
        title_confirm => "confirm", "確認";
        no_branch_bare => "the bare repo has no branch to sync",
                          "bare リポジトリには同期するブランチがありません";
        name_required => "name is required", "名前を入力してください";
        nothing_to_create => "nothing to create", "作成対象がありません";
    }}

    // ---- sync manager -----------------------------------------------------
    msg! { SYNC {
        sync_help => " ↑↓:nav  a:add  e:edit  d:remove  r:apply  f:filter  ?:keys  q:quit ",
                     " ↑↓:移動  a:追加  e:変更  d:削除  r:適用  f:絞込  ?:キー  q:終了 ";
        sync_help_filter => " type:filter  esc:clear  ↑↓:nav  enter:done ",
                            " 入力:絞込  esc:解除  ↑↓:移動  enter:確定 ";
        sync_help_kind => " l:link  c:copy  r:run  k:cache   ↑↓:move  enter:pick  esc/q:cancel ",
                          " l:リンク  c:コピー  r:コマンド  k:キャッシュ   enter:決定  esc/q:中止 ";
        sync_help_source => " type:filter  ↑↓/^p^n:nav  enter:choose file  esc:back ",
                            " 入力:絞込  ↑↓/^p^n:移動  enter:ファイル決定  esc:戻る ";
        sync_help_dest => " type:path inside each worktree  enter:apply now  esc:cancel ",
                          " 各ワークツリー内のパスを入力  enter:今すぐ適用  esc:中止 ";
        sync_help_dest_copy => " ^o:overwrite  ^r:render  enter:copy now  esc:cancel ",
                               " ^o:上書き  ^r:置換  enter:今すぐコピー  esc:中止 ";
        sync_help_cmd => " type a command  enter:register  esc:cancel ",
                         " コマンドを入力  enter:登録  esc:中止 ";
        sync_help_cache_path => " type the directory to cache  enter:next  esc:cancel ",
                                " キャッシュするディレクトリを入力  enter:次へ  esc:中止 ";
        sync_help_cache_mode => " ↑↓:move  enter:pick  k/s/p  esc/q:cancel ",
                                " ↑↓:移動  enter:決定  k/s/p  esc/q:中止 ";
        sync_help_cache_key => " space-separated files  enter:mount now  esc:cancel ",
                               " 空白区切りでファイルを列挙  enter:今すぐ適用  esc:中止 ";
        sync_help_remove => " y: remove the step and undo it   any other key: cancel ",
                            " y: 手順を削除して元に戻す   その他のキー: 中止 ";
        sync_title => "git wt sync", "git wt sync";
        sync_title_kind => "sync · new step", "sync · 手順の追加";
        sync_title_source => "sync · pick source", "sync · ソース選択";
        sync_title_dest => "sync · destination", "sync · 配置先";
        sync_title_cmd => "sync · command", "sync · コマンド";
        sync_title_cache => "sync · build cache", "sync · ビルドキャッシュ";
        sync_dest_sub => "in every worktree", "全ワークツリー内";
        col_kind => "KIND", "種別";
        col_source => "SOURCE (<repo-root>/…) or COMMAND",
                      "ソース (<リポジトリルート>/…) / コマンド";
        col_dest => "DEST (<worktree>/…)", "配置先 (<ワークツリー>/…)";
        col_state => "STATE", "状態";
        col_applied => "APPLIED", "適用";
        state_ok => "ok", "ok";
        state_missing => "MISSING", "無し";
        kind_link_desc => "symlink one real file into every worktree",
                          "実ファイルを全ワークツリーに symlink する";
        kind_copy_desc => "copy it instead — for files a tool rewrites in place",
                          "コピーする。ツールが書き換えるファイル向け";
        kind_run_desc => "run a command when a worktree is created",
                         "ワークツリー作成時にコマンドを実行する";
        kind_cache_desc => "keep a build cache outside the worktree, and share it safely",
                           "ビルドキャッシュをワークツリーの外に置いて安全に共有する";
        cache_mode_keyed_desc => "share only with worktrees whose key files match",
                                 "キーのファイルが一致するワークツリーとだけ共有";
        cache_mode_shared_desc => "one cache for the whole repo — for caches that cannot be poisoned",
                                  "リポジトリで 1 つ。壊れようがないキャッシュ向け";
        cache_mode_private_desc => "one per worktree, but it outlives the worktree",
                                   "ワークツリーごとに 1 つ。削除しても残る";
        cache_path_question => "which directory should live outside the worktree?",
                               "どのディレクトリをワークツリーの外に置きますか？";
        cache_path_hint => "relative to each worktree's root, e.g. ",
                           "各ワークツリーのルートからの相対パス。例: ";
        cache_path_hint2 => "the real data goes under ", "実体の置き場所は ";
        cache_key_hint => "files whose contents decide who shares, e.g. ",
                          "共有の可否を決めるファイル。例: ";
        cache_key_hint2 => "or several, space separated, e.g. ",
                           "複数可。空白区切り。例: ";
        cache_key_required => "at least one key file is required",
                              "キーとなるファイルを 1 つ以上入力してください";
        cache_detail_split => "the key has separated these worktrees",
                              "キーによってワークツリーが分かれています";
        cache_detail_together => "every worktree shares one bucket",
                                 "全ワークツリーが同じ実体を共有しています";
        label_cache_path => "cache dir", "キャッシュ先";
        label_cache_key => "key files", "キー";
        label_mounting => "mounting", "接続中";
        empty_title => "no sync steps yet.", "まだ手順がありません。";
        empty_hint_pre => "press ", "";
        empty_hint_post => " to link a file, copy one, or add a command.",
                           " を押すとリンク・コピー・コマンドを追加できます。";
        pick_source_hint => "pick the real file — paths are relative to ",
                            "実ファイルを選んでください — パスの基準は ";
        dest_question => "where should it land inside each worktree?",
                         "各ワークツリー内のどこに置きますか？";
        dest_relative_hint => "the path is relative to that worktree's root, e.g. ",
                              "そのワークツリーのルートからの相対パス。例: ";
        dest_required => "destination is required", "配置先を入力してください";
        cmd_question => "what should run inside a new worktree?",
                        "新しいワークツリーで何を実行しますか？";
        cmd_hint => "it runs through the shell, from the worktree root, e.g. ",
                    "ワークツリーのルートでシェル経由で実行します。例: ";
        cmd_required => "a command is required", "コマンドを入力してください";
        cmd_more_in_toml => "only_if, timeout and dir are set in .gwt/sync.toml",
                            "only_if・timeout・dir は .gwt/sync.toml で設定します";
        opt_overwrite => "overwrite", "上書き";
        opt_render => "render", "置換";
        label_source => "source", "ソース";
        label_dest => "dest (in each worktree)", "配置先 (各ワークツリー内)";
        label_command => "command", "コマンド";
        label_filter => "filter", "絞込";
        label_recipe => " recipe ", " 定義ファイル ";
        detail_applied_in => "applied in", "適用済み";
        detail_missing_in => "missing in", "未適用";
        detail_no_worktrees => "no worktrees yet", "ワークツリーがありません";
        detail_runs_on => "runs on", "実行タイミング";
        detail_only_if => "only if", "条件";
        detail_timeout => "timeout", "制限時間";
        no_candidates => "no files to pick under the repo root — put the real file there first",
                         "リポジトリルート直下に候補がありません。先に実ファイルを置いてください";
        label_applying => "applying", "適用中";
        label_linking => "linking", "リンク中";
        label_removing => "removing", "削除中";
        src_missing_note => "the source file does not exist yet",
                            "ソースファイルがまだ存在しません";
        src_untouched => "the source file itself is never deleted",
                         "ソースファイル自体は削除されません";
    }}

    // ---- runtime-formatted messages ---------------------------------------
    use super::pick;

    pub fn sync_applied_into(src: &str, dst: &str, n: usize) -> String {
        pick(
            format!("{src} → (worktree)/{dst}  · applied to {n}"),
            format!("{src} → (ワークツリー)/{dst}  · {n} 個に適用しました"),
        )
    }

    pub fn sync_registered_no_src(src: &str) -> String {
        pick(
            format!("{src} registered, but the source does not exist yet"),
            format!("{src} を登録しました（ソースはまだ存在しません）"),
        )
    }

    pub fn cache_key_question(path: &str) -> String {
        pick(
            format!("which files decide who may share '{path}'?"),
            format!("'{path}' を共有してよいかを，どのファイルで判定しますか？"),
        )
    }

    pub fn cache_mounted(path: &str, bucket: &str, n: usize) -> String {
        pick(
            format!("{path} → bucket {bucket}  · mounted in {n}"),
            format!("{path} → バケツ {bucket}  · {n} 個に接続しました"),
        )
    }

    pub fn sync_registered_cmd(cmd: &str) -> String {
        pick(
            format!("registered: {cmd} — it runs when a worktree is created"),
            format!("{cmd} を登録しました。ワークツリー作成時に実行します"),
        )
    }

    pub fn sync_removed(subject: &str, n: usize) -> String {
        pick(
            format!("removed {subject} · undone in {n}"),
            format!("{subject} を削除 · {n} 個で元に戻しました"),
        )
    }

    pub fn sync_kept_real(names: &str) -> String {
        pick(
            format!("  · kept a real file in {names}"),
            format!("  · {names} は実ファイルのため残しました"),
        )
    }

    pub fn sync_no_entry(subject: &str) -> String {
        pick(
            format!("no entry for {subject}"),
            format!("{subject} の登録がありません"),
        )
    }

    /// Enter was pressed but no shell wrapper is listening, so the directory
    /// did not change. Without this the picker looks broken and there is
    /// nothing on screen to explain it.
    pub fn cd_integration_missing(path: &str, shell: &str) -> String {
        let rc = match shell {
            "zsh" => "~/.zshrc",
            "fish" => "~/.config/fish/config.fish",
            _ => "~/.bashrc",
        };
        let line = if shell == "fish" {
            "git-wt shellinit fish | source".to_string()
        } else {
            format!("eval \"$(git-wt shellinit {shell})\"")
        };
        pick(
            format!(
                "git wt: shell integration is not active, so the directory was not changed.\n\
                 \x20       picked: {path}\n\
                 \x20       add to {rc}:  {line}\n\
                 \x20       then open a new shell, and use `gwt` or `git wt`."
            ),
            format!(
                "git wt: シェル連携が有効でないため、ディレクトリを移動できませんでした。\n\
                 \x20       選択したパス: {path}\n\
                 \x20       {rc} に追加してください:  {line}\n\
                 \x20       追加後、新しいシェルを開いて `gwt` か `git wt` を使ってください。"
            ),
        )
    }

    pub fn sync_applied_to(n: usize) -> String {
        pick(
            format!("applied to {n} worktree(s)"),
            format!("{n} 個のワークツリーに適用しました"),
        )
    }

    // ---- help overlay -----------------------------------------------------
    msg! { HELP {
        help_title => "keys", "キー一覧";
        help_close => " ?/esc/q: close   ↑↓: scroll ", " ?/esc/q: 閉じる   ↑↓: スクロール ";
        help_sec_nav => "navigate", "移動";
        help_sec_act => "actions", "操作";
        help_sec_sync => "sync", "同期";
        help_sec_danger => "destructive (always confirmed)", "破壊的操作 (必ず確認あり)";
        help_sec_other => "other", "その他";

        k_updown => "move up / down", "上/下へ移動";
        k_topbottom => "jump to top / bottom", "先頭/末尾へ";
        k_enter_cd => "cd into the selected worktree", "選択したワークツリーへ移動";
        k_select => "toggle multi-select", "複数選択の切替";
        k_select_all => "select all / clear", "全選択 / 解除";
        k_pull => "pull (fast-forward only)", "pull (fast-forward のみ)";
        k_push => "push to origin (asks first)", "origin へ push (確認あり)";
        k_del => "delete worktree", "ワークツリーを削除";
        k_force_del => "force delete worktree", "ワークツリーを強制削除";
        k_new => "new worktree from a base branch", "基点ブランチから新規作成";
        k_new_dir => "same, but choose the directory name too", "同上 + ディレクトリ名も指定";
        k_review => "review a remote branch", "リモートブランチをレビュー";
        k_filter => "filter the list", "一覧を絞り込む";
        k_quit => "close the picker", "ピッカーを閉じる";
        k_help => "show this help", "このヘルプを表示";

        k_sadd => "add a step: link a file, copy one, or run a command",
                  "手順を追加: リンク・コピー・コマンド実行";
        k_sedit => "change the selected step's destination or command",
                   "選択中の手順の配置先やコマンドを変更";
        k_sdel => "remove the step and undo it in every worktree",
                  "手順を削除し全ワークツリーで元に戻す";
        k_sapply => "re-apply the whole recipe", "定義を全ワークツリーに再適用";
        k_squit => "close the manager", "管理画面を閉じる";
    }}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(s: &str) -> usize {
        // Same rule the TUI uses: CJK is double-width.
        s.chars()
            .map(|c| {
                let cp = c as u32;
                let wide = (0x1100..=0x115F).contains(&cp)
                    || (0x2E80..=0xA4CF).contains(&cp)
                    || (0xAC00..=0xD7A3).contains(&cp)
                    || (0xF900..=0xFAFF).contains(&cp)
                    || (0xFF00..=0xFF60).contains(&cp)
                    || (0xFFE0..=0xFFE6).contains(&cp);
                if wide {
                    2
                } else {
                    1
                }
            })
            .sum()
    }

    /// Footer/help lines are rendered inside the window border. Japanese is
    /// twice as wide per character, so a translation that reads fine in a
    /// source file can silently run off the edge of a split pane.
    #[test]
    fn footer_lines_fit_a_narrow_terminal() {
        const BUDGET: usize = 96;
        for group in [t::SHARED, t::PICKER, t::SYNC, t::HELP] {
            for (name, en, ja) in group {
                // Convention: help/footer strings are padded with spaces.
                if !(en.starts_with(' ') && en.ends_with(' ')) {
                    continue;
                }
                for (lang, s) in [("en", en), ("ja", ja)] {
                    assert!(
                        cols(s) <= BUDGET,
                        "{name} ({lang}) is {} columns, over the {BUDGET} budget: {s:?}",
                        cols(s)
                    );
                }
            }
        }
    }

    #[test]
    fn every_entry_is_translated() {
        for group in [t::SHARED, t::PICKER, t::SYNC, t::HELP] {
            for (name, en, ja) in group {
                assert!(!en.trim().is_empty(), "{name} has no English text");
                // A handful are deliberately identical (product names, "ok").
                let same_ok = ["sync_title", "state_ok", "empty_hint_pre"];
                if !same_ok.contains(name) {
                    assert_ne!(en, ja, "{name} was never translated");
                }
            }
        }
    }

    #[test]
    fn parses_locale_strings() {
        assert_eq!(Lang::parse("ja_JP.UTF-8"), Some(Lang::Ja));
        assert_eq!(Lang::parse("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::parse("C"), Some(Lang::En));
        assert_eq!(Lang::parse("fr_FR"), None);
    }
}
