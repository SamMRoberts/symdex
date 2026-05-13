use std::{fs, path::Path, time::Instant};

use assert_cmd::Command;
use predicates::prelude::*;
use symdex::{
    config::{AppConfig, init_config},
    db::{self, RelationshipDirection},
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
fn config_init_refuses_existing_config_unless_forced() {
    let temp = tempdir().unwrap();
    init_config(temp.path(), false).unwrap();

    let error = init_config(temp.path(), false).unwrap_err();
    assert!(error.to_string().contains("already exists"));

    init_config(temp.path(), true).unwrap();
    assert!(temp.path().join("symdex.toml").is_file());
}

#[test]
fn local_config_overrides_project_config() {
    let temp = tempdir().unwrap();
    init_config(temp.path(), false).unwrap();
    fs::write(
        temp.path().join("symdex.local.toml"),
        "[storage]\ndatabase_path = '.symdex/local.db'\n\n[search]\nenable_fts = false\n\n[index]\nmax_file_size_bytes = 4\n",
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();
    assert_eq!(config.storage.database_path, Path::new(".symdex/local.db"));
    assert!(!config.search.enable_fts);
    assert_eq!(config.index.max_file_size_bytes, 4);
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

    let symbols = db::find_symbols(&conn, repo_id, "parse_config").unwrap();
    assert!(symbols.iter().any(|symbol| symbol.language == "python"));

    let imports = db::imports(&conn, repo_id, "src/main.py").unwrap();
    assert!(
        imports
            .iter()
            .any(|import| import.import_text.contains("pathlib"))
    );

    let callees = db::relationships(
        &conn,
        repo_id,
        "parse_config",
        RelationshipDirection::Callees,
    )
    .unwrap();
    assert!(callees.iter().any(|row| {
        row.relationship_kind == "calls"
            && row
                .evidence
                .as_deref()
                .is_some_and(|evidence| evidence.contains("read_text"))
    }));
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

#[test]
fn cli_structural_query_commands_return_contract_fields() {
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
        .args(["index", ".", "--watch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watch mode requested"));

    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Files indexed: 1"));
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["symbols", "in", "src/lib.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parse_config"));
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["refs", "read_file"])
        .assert()
        .success()
        .stdout(predicate::str::contains("call -> read_file"));
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["callers", "read_file"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parse_config"));
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["callees", "parse_config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("read_file"));
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["imports", "src/lib.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported_path="));
}

#[test]
fn cli_parse_error_and_unindexed_guidance_paths_work() {
    let python = fixture_project("python-basic");
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(python.path())
        .args(["init"])
        .assert()
        .success();
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(python.path())
        .args(["index", "."])
        .assert()
        .success();
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(python.path())
        .args(["errors", "--file", "src/bad.py"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tree-sitter parse error"));
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(python.path())
        .args(["files", "with-errors"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/bad.py"));

    let unindexed = tempdir().unwrap();
    init_config(unindexed.path(), false).unwrap();
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(unindexed.path())
        .args(["symbols", "find", "parse_config"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "repository has not been indexed yet",
        ));
}

#[test]
fn cli_reports_missing_config_guidance() {
    let temp = tempdir().unwrap();
    Command::cargo_bin("symdex")
        .unwrap()
        .current_dir(temp.path())
        .args(["status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `symdex init`"));
}

#[test]
fn queries_imports_relationships_status_and_errors() {
    let temp = fixture_project("rust-basic");
    init_config(temp.path(), false).unwrap();
    index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();
    let conn = db::open_database(temp.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, temp.path()).unwrap().unwrap();

    let status = db::status(
        &conn,
        temp.path(),
        &config.database_path(temp.path()),
        &config,
    )
    .unwrap();
    assert_eq!(status.files_indexed, 1);
    assert!(status.symbols_indexed >= 2);

    let imports = db::imports(&conn, repo_id, "src/lib.rs").unwrap();
    assert!(
        imports
            .iter()
            .any(|import| import.import_text.contains("fs"))
    );

    let callees = db::relationships(
        &conn,
        repo_id,
        "parse_config",
        RelationshipDirection::Callees,
    )
    .unwrap();
    assert!(callees.iter().any(|row| {
        row.relationship_kind == "calls"
            && row
                .evidence
                .as_deref()
                .is_some_and(|evidence| evidence.contains("read_file"))
    }));
}

#[test]
fn fts_search_is_used_when_enabled_and_sql_search_remains_available() {
    let temp = fixture_project("rust-basic");
    init_config(temp.path(), false).unwrap();
    index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();
    let conn = db::open_database(temp.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, temp.path()).unwrap().unwrap();

    let fts_matches = db::find_symbols_with_search(&conn, repo_id, "String", true).unwrap();
    assert!(fts_matches.iter().any(|row| row.matched_by == "fts"));

    let sql_matches = db::find_symbols_with_search(&conn, repo_id, "parse_config", false).unwrap();
    assert!(sql_matches.iter().any(|row| row.matched_by == "exact"));
}

#[test]
fn relationship_kinds_include_reference_import_and_dependency_evidence() {
    let temp = fixture_project("rust-basic");
    init_config(temp.path(), false).unwrap();
    index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();
    let conn = db::open_database(temp.path(), &config).unwrap();
    let mut stmt = conn
        .prepare("SELECT DISTINCT relationship_kind FROM symbol_relationships")
        .unwrap();
    let kinds = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()
        .unwrap();

    assert!(kinds.contains("calls"));
    assert!(kinds.contains("references"));
    assert!(kinds.contains("imports"));
    assert!(kinds.contains("depends_on"));
}

#[test]
fn deleted_and_skipped_files_are_excluded_from_active_results() {
    let temp = fixture_project("rust-basic");
    init_config(temp.path(), false).unwrap();
    index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();

    fs::remove_file(temp.path().join("src/lib.rs")).unwrap();
    let summary = index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert_eq!(summary.files_deleted, 1);

    let config = AppConfig::load(temp.path()).unwrap();
    let conn = db::open_database(temp.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, temp.path()).unwrap().unwrap();
    assert!(
        db::find_symbols(&conn, repo_id, "parse_config")
            .unwrap()
            .is_empty()
    );

    let skipped = tempdir().unwrap();
    fs::create_dir_all(skipped.path().join("src")).unwrap();
    fs::write(skipped.path().join("src/lib.rs"), "pub fn too_big() {}\n").unwrap();
    fs::write(skipped.path().join("src/notes.txt"), "unsupported\n").unwrap();
    init_config(skipped.path(), false).unwrap();
    fs::write(
        skipped.path().join("symdex.local.toml"),
        "[index]\nmax_file_size_bytes = 4\n",
    )
    .unwrap();
    let skipped_summary = index_repository(
        skipped.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert_eq!(skipped_summary.files_scanned, 0);
}

#[test]
fn indexes_typescript_and_javascript_fixtures() {
    let type_script = fixture_project("typescript-basic");
    init_config(type_script.path(), false).unwrap();
    let type_script_summary = index_repository(
        type_script.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert_eq!(type_script_summary.files_parsed, 1);
    assert!(type_script_summary.symbols_indexed >= 2);
    assert!(type_script_summary.imports_indexed >= 1);

    let config = AppConfig::load(type_script.path()).unwrap();
    let conn = db::open_database(type_script.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, type_script.path())
        .unwrap()
        .unwrap();
    let symbols = db::find_symbols(&conn, repo_id, "parseConfig").unwrap();
    assert!(symbols.iter().any(|symbol| symbol.language == "typescript"));
    let imports = db::imports(&conn, repo_id, "src/index.ts").unwrap();
    assert!(
        imports
            .iter()
            .any(|import| import.import_text.contains("node:fs"))
    );
    let callees = db::relationships(
        &conn,
        repo_id,
        "parseConfig",
        RelationshipDirection::Callees,
    )
    .unwrap();
    assert!(callees.iter().any(|row| {
        row.relationship_kind == "calls"
            && row
                .evidence
                .as_deref()
                .is_some_and(|evidence| evidence.contains("readFileSync"))
    }));

    let java_script = fixture_project("javascript-basic");
    init_config(java_script.path(), false).unwrap();
    let java_script_summary = index_repository(
        java_script.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();
    assert_eq!(java_script_summary.files_parsed, 1);
    assert!(java_script_summary.symbols_indexed >= 2);
    assert!(java_script_summary.imports_indexed >= 1);

    let config = AppConfig::load(java_script.path()).unwrap();
    let conn = db::open_database(java_script.path(), &config).unwrap();
    let repo_id = db::repository_id(&conn, java_script.path())
        .unwrap()
        .unwrap();
    let symbols = db::find_symbols(&conn, repo_id, "parseConfig").unwrap();
    assert!(symbols.iter().any(|symbol| symbol.language == "javascript"));
    let imports = db::imports(&conn, repo_id, "src/index.js").unwrap();
    assert!(
        imports
            .iter()
            .any(|import| import.import_text.contains("node:fs"))
    );
    let callees = db::relationships(
        &conn,
        repo_id,
        "parseConfig",
        RelationshipDirection::Callees,
    )
    .unwrap();
    assert!(callees.iter().any(|row| {
        row.relationship_kind == "calls"
            && row
                .evidence
                .as_deref()
                .is_some_and(|evidence| evidence.contains("readFileSync"))
    }));
}

#[test]
fn indexes_generated_thousand_file_fixture_under_goal() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for index in 0..1_000 {
        fs::write(
            src.join(format!("file_{index}.rs")),
            format!("pub fn generated_{index}() -> usize {{ {index} }}\n"),
        )
        .unwrap();
    }
    init_config(temp.path(), false).unwrap();

    let started = Instant::now();
    let summary = index_repository(
        temp.path(),
        IndexOptions {
            full: false,
            watch: false,
        },
    )
    .unwrap();

    assert_eq!(summary.files_parsed, 1_000);
    assert_eq!(summary.symbols_indexed, 1_000);
    assert!(started.elapsed().as_secs() < 30);
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
