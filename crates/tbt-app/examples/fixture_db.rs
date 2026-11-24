#[path = "../tests/common/mod.rs"]
mod common;

use std::path::PathBuf;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let dir = std::env::var_os(tbt_store::paths::DATA_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context(
            "set TBT_DATA_DIR to a scratch directory; this never writes to the default data dir",
        )?;
    let mut unpriced = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--unpriced" => unpriced.push(args.next().context("--unpriced needs a model id")?),
            other => {
                anyhow::bail!("unknown argument {other:?}; usage: fixture_db [--unpriced MODEL]...")
            }
        }
    }
    let unpriced: Vec<&str> = unpriced.iter().map(String::as_str).collect();
    std::fs::create_dir_all(&dir)?;
    let roots = common::copy_claude_code_fixtures(&dir.join("fixture-logs"))?;
    let db = tbt_store::paths::db_path(&dir);
    let n = common::build_db(&db, &roots, &unpriced, jiff::Timestamp::now())?;
    println!(
        "{}: {n} new events from {} roots",
        db.display(),
        roots.len()
    );
    Ok(())
}
