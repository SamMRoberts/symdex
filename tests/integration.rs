use std::{fs, path::Path};

use assert_cmd::Command;
use predicates::prelude::*;
use symdex::{
    config::{AppConfig, init_config},
    db,
    indexer::pipeline::{IndexOptions, index_repository},
};
use tempfile::tempdir;

#[test]
fn config_init_and_loads_defaults() {
    let temp = tempdir().unwrap();
    init_config(temp.path(), false).unwrap();
    let config = AppConfig::load(temp.path()).unwrap();
    assert_eq!(config.storage.database_path, Path::new(".symdex/index.db"));
    assert!(temp.path().join(".symdex").is_dir());
}

#[test]
fn indexes_rust_fixture_and_skips_unchanged_files() {
    let temp = fixture_project("rust-basic");
    init_config(temp.path(), false).unwrap();

    let first = index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert_eq!(first.files_parsed, 1);
    assert!(first.symbols_indexed >= 2);
    assert!(first.imports_indexed >= 1);

    let second = index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert_eq!(second.files_parsed, 0);
    assert_eq!(second.files_skipped, 1);

    let config = AppConfig::load(temp.path()).unwrap();
    let conn = db::open_database(temp.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, temp.path()).unwrap().unwrap();
    let symbols = db::find_symbols(&conn, repo_id, "parse_config").unwrap();
    assert_eq!(symbols[0].name, "parse_config");
    let refs = db::references(&conn, repo_id, "read_file").unwrap();
    assert!(!refs.is_empty());
}

#[test]
fn records_python_parse_errors() {
    let temp = fixture_project("python-basic");
    init_config(temp.path(), false).unwrap();
    let summary = index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert!(summary.parse_errors > 0);

    let config = AppConfig::load(temp.path()).unwrap();
    let conn = db::open_database(temp.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, temp.path()).unwrap().unwrap();
    let errors = db::parse_errors(&conn, repo_id, Some("src/bad.py")).unwrap();
    assert!(!errors.is_empty());
}

#[test]
fn cli_indexes_and_finds_symbols() {
    let temp = fixture_project("rust-basic");
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["init"])
        .assert()
        .success();
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["index", "."])
        .assert()
        .success();
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["symbols", "find", "parse_config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parse_config"));
}

fn fixture_project(name: &str) -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    copy_dir(
        Path::new("tests/fixtures").join(name).as_path(),
        temp.path(),
    );
    temp
}

fn copy_dir(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
