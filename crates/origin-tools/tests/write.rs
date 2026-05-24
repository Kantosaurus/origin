use origin_tools::builtins::write::write_tool;
use origin_tools::{registry_iter, SideEffects, Tier};
use std::fs;

#[test]
fn writes_new_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hello.txt");
    write_tool(path.to_str().expect("utf8"), "hi there").expect("write ok");
    let body = fs::read_to_string(&path).expect("read");
    assert_eq!(body, "hi there");
}

#[test]
fn creates_missing_parent_dirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a").join("b").join("c").join("nested.txt");
    write_tool(path.to_str().expect("utf8"), "deep").expect("write ok");
    let body = fs::read_to_string(&path).expect("read");
    assert_eq!(body, "deep");
}

#[test]
fn overwrites_existing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("file.txt");
    fs::write(&path, "old").expect("seed");
    write_tool(path.to_str().expect("utf8"), "new").expect("write ok");
    let body = fs::read_to_string(&path).expect("read");
    assert_eq!(body, "new");
}

#[test]
fn registers_in_inventory() {
    let meta = registry_iter()
        .find(|m| m.name == "Write")
        .expect("Write tool registered");
    assert_eq!(meta.tier, Tier::RequiresPermission);
    assert_eq!(meta.side_effects, SideEffects::Mutating);
    assert!(meta.input_schema.contains("\"path\""));
    assert!(meta.input_schema.contains("\"content\""));
}
