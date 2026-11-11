//! The examples are what bot authors copy. One that reaches into `vector_core` teaches the
//! habit the SDK exists to end, so each stays on the SDK's own surface, or says why not.

const MARKER: &str = "// example-uses-core:";

#[test]
fn examples_never_reach_into_core() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let excused = src.lines().any(|l| l.trim_start().strip_prefix(MARKER).is_some_and(|why| why.trim().len() > 8));
        if excused {
            continue;
        }
        for (n, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains(".core()") || line.contains("vector_core::") || line.contains("VectorCore") {
                offenders.push(format!("{}:{}: {}", path.file_name().unwrap().to_string_lossy(), n + 1, line.trim()));
            }
        }
    }
    assert!(offenders.is_empty(), "examples use the SDK, not vector_core (or carry `{MARKER} <why>`):\n{}", offenders.join("\n"));
}
