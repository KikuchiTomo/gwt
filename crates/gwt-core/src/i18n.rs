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
    ($($(#[$m:meta])* $name:ident => $en:expr, $ja:expr;)*) => {
        $(
            $(#[$m])*
            pub fn $name() -> &'static str {
                match $crate::i18n::current() {
                    $crate::i18n::Lang::En => $en,
                    $crate::i18n::Lang::Ja => $ja,
                }
            }
        )*
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
    msg! {
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
    }

    // ---- worktree picker --------------------------------------------------
    msg! {
        picker_help => " j/k ↑↓:nav  tab:select  enter:cd  p:pull  P:push  d:del  D:force-del  e/n:new  r:review  f:filter  ?:help  q:quit ",
                       " j/k ↑↓:移動  tab:選択  enter:移動する  p:pull  P:push  d:削除  D:強制削除  e/n:新規  r:レビュー  f:絞込  ?:ヘルプ  q:終了 ";
        picker_help_filter => " type:filter  esc:exit  ↑↓/^p^n/^j^k:nav  tab:select  enter:cd ",
                              " 入力:絞込  esc:解除  ↑↓/^p^n/^j^k:移動  tab:選択  enter:移動する ";
        picker_help_selected => " tab/space:select  a:all  d:del  D:force-del  esc:clear  enter:cd ",
                                " tab/space:選択  a:全選択  d:削除  D:強制削除  esc:選択解除  enter:移動する ";
        filter_prompt_hint => " press f or / to filter ", " f または / で絞り込み ";
        conflict_help => " [key]:choose  ↑↓:move  enter:pick  esc:cancel ",
                         " [キー]:選択  ↑↓:移動  enter:決定  esc:中止 ";
        push_confirm_help => " y: push to remote   any other key: cancel ",
                             " y: リモートへ push   その他のキー: 中止 ";
        pulling => " pulling… ", " pull 中… ";
        pushing => " pushing… ", " push 中… ";
        deleting => " deleting… ", " 削除中… ";
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
    }

    // ---- secrets manager --------------------------------------------------
    msg! {
        secret_help => " j/k ↑↓:nav  a:add  e:re-point  d:remove  r:relink  f:filter  ?:help  q:quit ",
                       " j/k ↑↓:移動  a:追加  e:変更  d:削除  r:再リンク  f:絞込  ?:ヘルプ  q:終了 ";
        secret_help_filter => " type:filter  esc:clear  ↑↓:nav  enter:done ",
                              " 入力:絞込  esc:解除  ↑↓:移動  enter:確定 ";
        secret_help_source => " type:filter  ↑↓/^p^n:nav  enter:choose file  esc:back ",
                              " 入力:絞込  ↑↓/^p^n:移動  enter:ファイル決定  esc:戻る ";
        secret_help_dest => " type:path inside each worktree  enter:link now  esc:cancel ",
                            " 各ワークツリー内のパスを入力  enter:リンク作成  esc:中止 ";
        secret_help_remove => " y: remove mapping + its links   any other key: cancel ",
                              " y: 対応付けとリンクを削除   その他のキー: 中止 ";
        secret_title => "git wt secret", "git wt secret";
        secret_title_source => "secret · pick source", "secret · ソース選択";
        secret_title_dest => "secret · destination", "secret · 配置先";
        secret_dest_sub => "in every worktree", "全ワークツリー内";
        col_source => "SOURCE (<repo-root>/…)", "ソース (<リポジトリルート>/…)";
        col_dest => "DEST (<worktree>/…)", "配置先 (<ワークツリー>/…)";
        col_state => "STATE", "状態";
        col_linked => "LINKED", "リンク";
        state_ok => "ok", "ok";
        state_missing => "MISSING", "無し";
        empty_title => "no secret mappings yet.", "まだ登録がありません。";
        empty_hint_pre => "press ", "";
        empty_hint_post => " to pick a file and link it into every worktree.",
                           " を押すとファイルを選んで全ワークツリーにリンクできます。";
        pick_source_hint => "pick the real file — paths are relative to ",
                            "実ファイルを選んでください — パスの基準は ";
        dest_question => "where should the link appear inside each worktree?",
                         "各ワークツリー内のどこにリンクを置きますか？";
        dest_relative_hint => "the path is relative to that worktree's root, e.g. ",
                              "そのワークツリーのルートからの相対パス。例: ";
        dest_required => "destination is required", "配置先を入力してください";
        label_source => "source", "ソース";
        label_dest => "dest (in each worktree)", "配置先 (各ワークツリー内)";
        label_filter => "filter", "絞込";
        label_manifest => " manifest ", " 定義ファイル ";
        detail_linked_in => "linked in", "リンク済み";
        detail_missing_in => "missing in", "未リンク";
        detail_no_worktrees => "no worktrees yet", "ワークツリーがありません";
        detail_foreign => "a real file is in the way", "実ファイルが存在します";
        no_candidates => "no files to pick under the repo root — put the real file there first",
                         "リポジトリルート直下に候補がありません。先に実ファイルを置いてください";
        label_relinking => "relinking", "再リンク中";
        label_linking => "linking", "リンク中";
        label_removing => "removing", "削除中";
        secret_confirm_remove_pre => "remove '", "'";
        secret_confirm_remove_mid => "' and unlink ", "' の対応付けを削除し ";
        secret_confirm_remove_post => " everywhere ? y/N", " のリンクを全て解除しますか？ y/N";
        src_missing_note => "the source file does not exist yet",
                            "ソースファイルがまだ存在しません";
        src_untouched => "the source file itself is never deleted",
                         "ソースファイル自体は削除されません";
    }

    // ---- runtime-formatted messages ---------------------------------------
    use super::pick;

    pub fn secret_linked_into(src: &str, dst: &str, n: usize) -> String {
        pick(
            format!("{src} → (worktree)/{dst}  · linked into {n}"),
            format!("{src} → (ワークツリー)/{dst}  · {n} 個にリンクしました"),
        )
    }

    pub fn secret_registered_no_src(src: &str) -> String {
        pick(
            format!("{src} registered, but the source does not exist yet"),
            format!("{src} を登録しました（ソースはまだ存在しません）"),
        )
    }

    pub fn secret_removed(src: &str, n: usize) -> String {
        pick(
            format!("removed {src} · unlinked {n}"),
            format!("{src} を削除 · {n} 個のリンクを解除"),
        )
    }

    pub fn secret_kept_real(names: &str) -> String {
        pick(
            format!("  · kept a real file in {names}"),
            format!("  · {names} は実ファイルのため残しました"),
        )
    }

    pub fn secret_no_entry(src: &str) -> String {
        pick(
            format!("no entry for {src}"),
            format!("{src} の登録がありません"),
        )
    }

    pub fn relinked(n: usize) -> String {
        pick(
            format!("relinked {n} worktree(s)"),
            format!("{n} 個のワークツリーを再リンクしました"),
        )
    }

    // ---- help overlay -----------------------------------------------------
    msg! {
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

        k_sadd => "add a mapping (pick a file, then the destination)",
                  "対応付けを追加 (ファイルを選び、配置先を入力)";
        k_sedit => "re-point the selected mapping to a new destination",
                   "選択中の配置先を変更";
        k_sdel => "remove the mapping and unlink it everywhere",
                  "対応付けを削除し全ワークツリーのリンクも解除";
        k_srelink => "re-apply every link", "全リンクを貼り直す";
        k_squit => "close the manager", "管理画面を閉じる";
    }
}
