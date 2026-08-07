use anyhow::Result;
use gwt_core::i18n::{self, Lang};

/// `git wt config` shows the resolved language and where it came from;
/// `git wt config lang <code>` persists a choice.
pub fn run(set_lang: Option<&str>) -> Result<()> {
    if let Some(code) = set_lang {
        let lang = Lang::parse(code)
            .ok_or_else(|| anyhow::anyhow!("unknown language '{code}' (expected en or ja)"))?;
        let path = i18n::write_config_lang(lang)?;
        // Re-resolve so the confirmation prints in the language just chosen —
        // unless something more specific (a flag, $GWT_LANG) still outranks it.
        eprintln!("lang = {} ({})", lang.code(), lang.label());
        eprintln!("saved to {}", path.display());
        let effective = i18n::detect(None);
        if effective != lang {
            eprintln!(
                "note: $GWT_LANG is set to '{}' and takes precedence over this file",
                effective.code()
            );
        }
        return Ok(());
    }

    let current = i18n::current();
    println!("lang = {}  ({})", current.code(), current.label());
    println!(
        "available: {}",
        Lang::ALL
            .iter()
            .map(|l| format!("{} ({})", l.code(), l.label()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(p) = i18n::config_path() {
        println!("config: {}", p.display());
    }
    println!("override order: --lang > $GWT_LANG > config > $LC_ALL/$LC_MESSAGES/$LANG");
    Ok(())
}
