//! Render already-evaluated findings without running or reinterpreting any check.

use std::collections::BTreeMap;

use crate::findings::{Finding, Folder, Severity, selection::Kind};

/// Render shared findings, hiding informational brackets unless all details are requested.
pub fn render(tree: &Folder, uri: &str, findings: &[Finding], color: bool, all: bool) -> String {
    let mut by_uri: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for finding in findings {
        by_uri.entry(&finding.uri).or_default().push(finding);
    }
    let mut output = String::new();
    render_into(
        tree,
        uri,
        uri,
        "",
        None,
        &RenderOptions { by_uri, color, all },
        &mut output,
    );
    output
}

/// Immutable lookup and formatting shared through tree traversal, not stored on folders.
struct RenderOptions<'a> {
    by_uri: BTreeMap<&'a str, Vec<&'a Finding>>,
    color: bool,
    all: bool,
}

/// Format one node and collapse repositories and folders with unconfigured findings.
fn render_into(
    tree: &Folder,
    uri: &str,
    name: &str,
    indent: &str,
    last: Option<bool>,
    options: &RenderOptions<'_>,
    output: &mut String,
) {
    let findings = options
        .by_uri
        .get(uri)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let severity = findings
        .iter()
        .map(|finding| finding.severity)
        .max()
        .unwrap_or(Severity::Neutral);
    let missing = findings
        .iter()
        .any(|finding| finding.check == Kind::Folders && finding.fatal);
    let unconfigured = tree.repository.is_none()
        && findings
            .iter()
            .any(|finding| finding.check == Kind::Folders && finding.is_issue());
    let style = match severity {
        Severity::Neutral => "",
        Severity::Passed => "32",
        Severity::Warning => "33",
        Severity::Error if missing => "31;4",
        Severity::Error => "31",
    };
    let mut detail = findings
        .iter()
        .filter(|finding| options.all || finding.is_issue())
        .map(|finding| format!("  [{}: {}]", finding.check.name(), finding.message))
        .collect::<String>();
    let collapsed = tree.repository.is_some() || unconfigured;
    if collapsed || tree.children.is_empty() {
        detail.push_str(&format!(" ({} files)", tree.file_count));
    }
    output.push_str(indent);
    if let Some(last) = last {
        output.push_str(if last { "└── " } else { "├── " });
    }
    if options.color && !style.is_empty() {
        output.push_str(&format!("\x1b[{style}m{name}{detail}\x1b[0m\n"));
    } else {
        output.push_str(&format!("{name}{detail}\n"));
    }
    if collapsed {
        return;
    }
    let child_indent = format!(
        "{indent}{}",
        match last {
            Some(true) => "    ",
            Some(false) => "│   ",
            None => "",
        }
    );
    for (index, (name, child)) in tree.children.iter().enumerate() {
        render_into(
            child,
            &format!("{uri}/{name}"),
            name,
            &child_indent,
            Some(index + 1 == tree.children.len()),
            options,
            output,
        );
    }
}
