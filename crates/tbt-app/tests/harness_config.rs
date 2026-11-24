mod common;

use std::path::PathBuf;

use tbt_app::data::{DataSource, StoreData};
use tbt_app::pages::harnesses::{HarnessEdit, HarnessesData, apply_edit, check_roots, parse_roots};
use tbt_app::pages::projects::{ProjectSort, ProjectsData, branch_note, sort_projects};
use tbt_core::{HarnessId, TimeRange};
use tbt_store::{BucketSize, QueryFilter, Store};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    db: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let roots = common::copy_claude_code_fixtures(&dir.path().join("logs")).unwrap();
    let db = dir.path().join("data").join("tbt.sqlite");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    common::build_db(&db, &roots, &[], "2026-11-23T00:00:00Z".parse().unwrap()).unwrap();
    Fixture { dir, db }
}

fn config(db: &std::path::Path, id: &HarnessId) -> tbt_store::HarnessConfig {
    Store::open_read_only(db)
        .unwrap()
        .harnesses()
        .unwrap()
        .into_iter()
        .find(|h| h.id == *id)
        .unwrap()
}

#[test]
fn toggle_and_roots_round_trip_through_the_store() {
    let f = fixture();
    let id = HarnessId::CLAUDE_CODE;
    assert!(config(&f.db, &id).enabled);
    assert_eq!(config(&f.db, &id).roots, None);

    apply_edit(&f.db, &id, &HarnessEdit::Enabled(false)).unwrap();
    assert!(!config(&f.db, &id).enabled);
    apply_edit(&f.db, &id, &HarnessEdit::Enabled(true)).unwrap();
    assert!(config(&f.db, &id).enabled);

    let existing = f.dir.path().join("logs");
    let text = format!(
        "{}\n\n{}/missing\n{}\n",
        existing.display(),
        existing.display(),
        existing.display()
    );
    let roots = parse_roots(&text, None);
    assert_eq!(roots.len(), 2);
    let issues = check_roots(&roots);
    assert_eq!(issues.len(), 1);
    assert!(!issues[0].blocks_save());

    apply_edit(&f.db, &id, &HarnessEdit::Roots(Some(roots.clone()))).unwrap();
    assert_eq!(config(&f.db, &id).roots, Some(roots.clone()));

    let data = StoreData::new(f.db.clone());
    assert_eq!(DataSource::db_path(&data), Some(f.db.clone()));
    let loaded = HarnessesData::load(&data).unwrap();
    let card = loaded
        .cards
        .iter()
        .find(|c| c.status.config.id == id)
        .unwrap();
    assert!(card.custom);
    assert_eq!(
        card.roots,
        vec![(roots[0].clone(), true), (roots[1].clone(), false)]
    );
    assert!(!card.defaults.is_empty());
    assert!(card.status.files_tracked > 0);
    assert!(!loaded.runs.is_empty());

    apply_edit(&f.db, &id, &HarnessEdit::Roots(None)).unwrap();
    assert_eq!(config(&f.db, &id).roots, None);
    let loaded = HarnessesData::load(&data).unwrap();
    let card = loaded
        .cards
        .iter()
        .find(|c| c.status.config.id == id)
        .unwrap();
    assert!(!card.custom);
    assert_eq!(
        card.roots
            .iter()
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>(),
        card.defaults
    );
}

#[test]
fn unknown_harness_edit_fails_without_side_effects() {
    let f = fixture();
    let ghost = HarnessId::new("ghost");
    assert!(apply_edit(&f.db, &ghost, &HarnessEdit::Enabled(false)).is_err());
    assert!(config(&f.db, &HarnessId::CLAUDE_CODE).enabled);
}

#[test]
fn projects_from_store_include_branches() {
    let f = fixture();
    let data = StoreData::new(f.db.clone());
    let filter = QueryFilter::new(
        TimeRange::new(
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "2027-01-01T00:00:00Z".parse().unwrap(),
        ),
        BucketSize::Day,
    );
    let d = ProjectsData::load(&data, &filter).unwrap();
    assert!(!d.no_data);
    assert!(!d.projects.is_empty());
    let kpi = data.kpi_summary(&filter).unwrap();
    let total: i64 = d.projects.iter().map(|p| p.cost.0).sum();
    assert_eq!(total, kpi.cost.0);
    let branch_total: i64 = d
        .projects
        .iter()
        .flat_map(|p| p.branches.iter())
        .map(|b| b.cost.0)
        .sum();
    assert_eq!(branch_total, kpi.cost.0);
    assert!(d.projects.iter().any(|p| branch_note(p).is_some()));
    let order = sort_projects(&d.projects, ProjectSort::Cost, true);
    assert!(
        order
            .windows(2)
            .all(|w| d.projects[w[0]].cost >= d.projects[w[1]].cost)
    );
}
