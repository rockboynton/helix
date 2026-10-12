use super::{configuration_directories, run_configuration_directory};
use steel::{steel_vm::engine::Engine, SteelVal};

fn config(root: &std::path::Path, name: &str, value: &str) -> std::path::PathBuf {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("settings.scm"),
        format!("(provide value) (define value \"{value}\")"),
    )
    .unwrap();
    std::fs::write(
        directory.join("helix.scm"),
        "(require \"settings.scm\") (provide configure!) (define (configure!) value)",
    )
    .unwrap();
    std::fs::write(
        directory.join("init.scm"),
        "(require \"helix.scm\") (set! events (append events (list (configure!))))",
    )
    .unwrap();
    directory
}

#[test]
fn user_then_project_preserves_relative_imports_and_same_module_names() {
    let root = tempfile::tempdir().unwrap();
    let user = config(root.path(), "user", "user");
    let project = config(root.path(), "project", "project");
    let mut engine = Engine::new();
    engine.run("(define events '())").unwrap();
    for directory in configuration_directories(user, project, true) {
        run_configuration_directory(&directory, |source, path| {
            engine
                .compile_and_run_raw_program_with_path(source.to_owned(), path)
                .map(|_| ())
        })
        .unwrap();
    }
    let result = engine
        .run("(equal? events '(\"user\" \"project\"))")
        .unwrap();
    assert_eq!(result.last(), Some(&SteelVal::BoolV(true)));
}

#[test]
fn untrusted_and_incomplete_projects_are_not_loaded() {
    let root = tempfile::tempdir().unwrap();
    let user = config(root.path(), "user", "user");
    let project = config(root.path(), "project", "project");
    assert_eq!(
        configuration_directories(user.clone(), project.clone(), false),
        vec![user.clone()]
    );
    std::fs::remove_file(project.join("init.scm")).unwrap();
    assert_eq!(
        configuration_directories(user.clone(), project, true),
        vec![user]
    );
}

#[test]
fn identical_directories_only_run_once() {
    let root = tempfile::tempdir().unwrap();
    let user = config(root.path(), "user", "user");
    assert_eq!(
        configuration_directories(user.clone(), user.clone(), true),
        vec![user]
    );
}

#[test]
fn absent_user_config_does_not_create_files_or_prevent_project_loading() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    run_configuration_directory(&missing, |_, _| panic!("no sources to run")).unwrap();
    assert!(!missing.exists());
    let project = config(root.path(), "project", "project");
    assert_eq!(
        configuration_directories(missing.clone(), project.clone(), true),
        vec![missing, project]
    );
}
