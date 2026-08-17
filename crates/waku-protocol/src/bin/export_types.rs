use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ts_rs::{Config, TS};
use waku_protocol::{
    ClientMessage, DaemonReady, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, ServerMessage,
    automation::Automation,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = generated_output();
    if std::env::args().nth(1).as_deref() == Some("--check") {
        check_generated(&output)?;
        println!("TypeScript protocol bindings are current");
        return Ok(());
    }
    export_to(&output)?;
    println!(
        "generated TypeScript protocol bindings in {}",
        output.display()
    );
    Ok(())
}

fn generated_output() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/waku-client/src/generated")
}

fn export_to(output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if output.exists() {
        fs::remove_dir_all(output)?;
    }
    fs::create_dir_all(output)?;

    let config = Config::new().with_out_dir(output).with_large_int("number");
    ClientMessage::export_all(&config)?;
    ServerMessage::export_all(&config)?;
    DaemonReady::export_all(&config)?;
    Automation::export_all(&config)?;
    strip_trailing_whitespace(output)?;
    fs::write(
        output.join("constants.ts"),
        format!(
            "// Generated from waku-protocol. Do not edit.\n\
             export const PROTOCOL_VERSION = {PROTOCOL_VERSION} as const;\n\
             export const MAX_WIRE_MESSAGE_BYTES = {MAX_WIRE_MESSAGE_BYTES} as const;\n"
        ),
    )?;
    write_index(output)?;
    Ok(())
}

fn strip_trailing_whitespace(root: &Path) -> std::io::Result<()> {
    fn visit(directory: &Path) -> std::io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path)?;
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("ts") {
                let source = fs::read_to_string(&path)?;
                let mut normalized = source
                    .lines()
                    .map(str::trim_end)
                    .collect::<Vec<_>>()
                    .join("\n");
                normalized.push('\n');
                fs::write(path, normalized)?;
            }
        }
        Ok(())
    }

    visit(root)
}

fn check_generated(expected: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let temporary = std::env::temp_dir().join(format!(
        "waku-protocol-bindings-{}",
        uuid::Uuid::new_v4().simple()
    ));
    export_to(&temporary)?;
    let actual_files = read_tree(&temporary)?;
    let expected_files = read_tree(expected)?;
    fs::remove_dir_all(&temporary)?;
    if actual_files != expected_files {
        return Err("generated bindings are stale; run `bun run protocol:generate`".into());
    }
    Ok(())
}

fn read_tree(root: &Path) -> std::io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(root, &path, files)?;
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap_or(&path).to_owned(),
                    fs::read(path)?,
                );
            }
        }
        Ok(())
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn write_index(output: &Path) -> std::io::Result<()> {
    let mut modules = fs::read_dir(output)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("ts"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .filter(|module| module != "constants" && module != "index")
        .collect::<Vec<_>>();
    modules.sort();
    let type_exports = modules
        .into_iter()
        .map(|module| format!("export type {{ {module} }} from \"./{module}\";\n"))
        .collect::<String>();
    let contents = format!(
        "export {{ MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION }} from \"./constants\";\n\
         export type {{ JsonValue }} from \"./serde_json/JsonValue\";\n\
         {type_exports}"
    );
    fs::write(PathBuf::from(output).join("index.ts"), contents)
}
