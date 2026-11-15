use mrt_core::{CommandBlock, StructuredOutput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeType {
    Add,
    Remove,
    Equal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonFieldChange {
    pub path: String,
    pub old_val: String,
    pub new_val: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuredDiffKind {
    Identical {
        summary: String,
    },
    Json {
        added_paths: Vec<String>,
        removed_paths: Vec<String>,
        modified_paths: Vec<JsonFieldChange>,
        diff_lines: Vec<(String, ChangeType)>,
    },
    PortsDelta {
        added_listeners: Vec<String>,
        removed_listeners: Vec<String>,
        unchanged_count: usize,
    },
    /// Two `:ps` / `:top` snapshots: processes started, exited, or whose memory grew.
    ProcessesDelta {
        started: Vec<String>,
        exited: Vec<String>,
        memory_grown: Vec<String>,
        unchanged_count: usize,
    },
    Text {
        diff_lines: Vec<(String, ChangeType)>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredDiffReport {
    pub previous_cmd: String,
    pub current_cmd: String,
    pub kind: StructuredDiffKind,
}

/// Recursively sorts all JSON object keys using BTreeMap so that two JSONs
/// with different key orders compare identically.
pub fn canonicalize_json(val: &Value) -> Value {
    match val {
        Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k.clone(), canonicalize_json(v));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => {
            let sorted: Vec<Value> = arr.iter().map(canonicalize_json).collect();
            Value::Array(sorted)
        }
        other => other.clone(),
    }
}

/// Discovers added, removed, and modified paths between two JSON values.
pub fn diff_json_paths(
    v1: &Value,
    v2: &Value,
    prefix: &str,
    added: &mut Vec<String>,
    removed: &mut Vec<String>,
    modified: &mut Vec<JsonFieldChange>,
) {
    match (v1, v2) {
        (Value::Object(m1), Value::Object(m2)) => {
            let keys1: HashSet<&String> = m1.keys().collect();
            let keys2: HashSet<&String> = m2.keys().collect();

            for k in keys2.difference(&keys1) {
                let path = if prefix.is_empty() { (*k).clone() } else { format!("{prefix}.{k}") };
                added.push(path);
            }
            for k in keys1.difference(&keys2) {
                let path = if prefix.is_empty() { (*k).clone() } else { format!("{prefix}.{k}") };
                removed.push(path);
            }
            for k in keys1.intersection(&keys2) {
                let path = if prefix.is_empty() { (*k).clone() } else { format!("{prefix}.{k}") };
                diff_json_paths(&m1[*k], &m2[*k], &path, added, removed, modified);
            }
        }
        (Value::Array(a1), Value::Array(a2)) => {
            if a1 != a2 {
                modified.push(JsonFieldChange {
                    path: prefix.to_string(),
                    old_val: format!("Array (len {})", a1.len()),
                    new_val: format!("Array (len {})", a2.len()),
                });
            }
        }
        (other1, other2) => {
            if other1 != other2 {
                modified.push(JsonFieldChange {
                    path: prefix.to_string(),
                    old_val: other1.to_string(),
                    new_val: other2.to_string(),
                });
            }
        }
    }
}

/// Compares two command blocks with awareness of JSON and port snapshots.
pub fn compare_blocks_structured(prev: &CommandBlock, curr: &CommandBlock) -> StructuredDiffReport {
    // 0. Process snapshots (`:ps` / `:top`)
    if crate::process_inspector::is_process_block(&prev.command, &prev.shell_name)
        && crate::process_inspector::is_process_block(&curr.command, &curr.shell_name)
    {
        if let (Some(before), Some(after)) = (
            crate::process_inspector::entries_from_table(&prev.structured),
            crate::process_inspector::entries_from_table(&curr.structured),
        ) {
            let report = crate::process_inspector::compare_process_snapshots(&before, &after);
            let kind = if report.is_empty() {
                StructuredDiffKind::Identical {
                    summary: format!(
                        "Process snapshots identical ({} processes unchanged).",
                        report.unchanged_count
                    ),
                }
            } else {
                StructuredDiffKind::ProcessesDelta {
                    started: report.started_lines(),
                    exited: report.exited_lines(),
                    memory_grown: report.memory_lines(),
                    unchanged_count: report.unchanged_count,
                }
            };
            return StructuredDiffReport {
                previous_cmd: prev.command.clone(),
                current_cmd: curr.command.clone(),
                kind,
            };
        }
    }

    // 1. Check for JSON comparison
    let prev_json = match &prev.structured {
        StructuredOutput::Json(v) => Some(v.clone()),
        _ => serde_json::from_str::<Value>(&prev.plain_output).ok(),
    };
    let curr_json = match &curr.structured {
        StructuredOutput::Json(v) => Some(v.clone()),
        _ => serde_json::from_str::<Value>(&curr.plain_output).ok(),
    };

    if let (Some(j1), Some(j2)) = (prev_json, curr_json) {
        let c1 = canonicalize_json(&j1);
        let c2 = canonicalize_json(&j2);

        if c1 == c2 {
            return StructuredDiffReport {
                previous_cmd: prev.command.clone(),
                current_cmd: curr.command.clone(),
                kind: StructuredDiffKind::Identical {
                    summary: "JSON structures and values are identical (key ordering differences ignored).".into(),
                },
            };
        }

        let mut added_paths = Vec::new();
        let mut removed_paths = Vec::new();
        let mut modified_paths = Vec::new();
        diff_json_paths(&c1, &c2, "", &mut added_paths, &mut removed_paths, &mut modified_paths);

        let pretty1 = serde_json::to_string_pretty(&c1).unwrap_or_default();
        let pretty2 = serde_json::to_string_pretty(&c2).unwrap_or_default();
        let diff_lines = compute_line_diff(&pretty1, &pretty2);

        return StructuredDiffReport {
            previous_cmd: prev.command.clone(),
            current_cmd: curr.command.clone(),
            kind: StructuredDiffKind::Json {
                added_paths,
                removed_paths,
                modified_paths,
                diff_lines,
            },
        };
    }

    // 2. Check for `:ports` / network table snapshots
    let prev_is_ports = prev.command.starts_with(":ports") || prev.shell_name == "mrt-net";
    let curr_is_ports = curr.command.starts_with(":ports") || curr.shell_name == "mrt-net";

    if prev_is_ports && curr_is_ports {
        let extract_listeners = |b: &CommandBlock| -> HashSet<String> {
            let mut set = HashSet::new();
            if let StructuredOutput::Table { rows, .. } = &b.structured {
                for r in rows {
                    if r.len() >= 3 {
                        let proto = &r[0];
                        let local = &r[1];
                        let port = &r[2];
                        let pid = if r.len() >= 5 { format!(" (PID {})", r[4]) } else { String::new() };
                        set.insert(format!("{proto} {local}:{port}{pid}"));
                    }
                }
            } else {
                for line in b.plain_output.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with("Proto") && !trimmed.starts_with("---") {
                        set.insert(trimmed.to_string());
                    }
                }
            }
            set
        };

        let set1 = extract_listeners(prev);
        let set2 = extract_listeners(curr);

        let mut added: Vec<String> = set2.difference(&set1).cloned().collect();
        let mut removed: Vec<String> = set1.difference(&set2).cloned().collect();
        let unchanged = set1.intersection(&set2).count();

        added.sort();
        removed.sort();

        if added.is_empty() && removed.is_empty() {
            return StructuredDiffReport {
                previous_cmd: prev.command.clone(),
                current_cmd: curr.command.clone(),
                kind: StructuredDiffKind::Identical {
                    summary: format!("Port listening snapshot identical ({unchanged} active listeners unchanged)."),
                },
            };
        }

        return StructuredDiffReport {
            previous_cmd: prev.command.clone(),
            current_cmd: curr.command.clone(),
            kind: StructuredDiffKind::PortsDelta {
                added_listeners: added,
                removed_listeners: removed,
                unchanged_count: unchanged,
            },
        };
    }

    // 3. Fallback to standard line diff
    if prev.plain_output == curr.plain_output {
        return StructuredDiffReport {
            previous_cmd: prev.command.clone(),
            current_cmd: curr.command.clone(),
            kind: StructuredDiffKind::Identical {
                summary: "Outputs are identical.".into(),
            },
        };
    }

    let diff_lines = compute_line_diff(&prev.plain_output, &curr.plain_output);
    StructuredDiffReport {
        previous_cmd: prev.command.clone(),
        current_cmd: curr.command.clone(),
        kind: StructuredDiffKind::Text { diff_lines },
    }
}

fn compute_line_diff(s1: &str, s2: &str) -> Vec<(String, ChangeType)> {
    let mut lines = Vec::new();
    for change in similar::TextDiff::from_lines(s1, s2).iter_all_changes() {
        let tag = match change.tag() {
            similar::ChangeTag::Delete => ChangeType::Remove,
            similar::ChangeTag::Insert => ChangeType::Add,
            similar::ChangeTag::Equal => ChangeType::Equal,
        };
        lines.push((change.value().trim_end_matches(['\r', '\n']).to_string(), tag));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_canonicalize_json_ignores_key_order() {
        let j1 = json!({ "b": 2, "a": 1, "sub": { "z": 9, "y": 8 } });
        let j2 = json!({ "a": 1, "b": 2, "sub": { "y": 8, "z": 9 } });

        let c1 = canonicalize_json(&j1);
        let c2 = canonicalize_json(&j2);

        assert_eq!(c1, c2);
    }

    #[test]
    fn test_compare_json_identical_with_reordered_keys() {
        let b1 = CommandBlock::new_completed("api_call", "pwsh", ".", r#"{"b":2,"a":1}"#, 0, 10);
        let b2 = CommandBlock::new_completed("api_call", "pwsh", ".", r#"{"a":1,"b":2}"#, 0, 10);

        let report = compare_blocks_structured(&b1, &b2);
        match report.kind {
            StructuredDiffKind::Identical { .. } => {}
            _ => panic!("Expected Identical report for reordered JSON"),
        }
    }

    #[test]
    fn test_compare_json_detects_changes() {
        let b1 = CommandBlock::new_completed("api", "pwsh", ".", r#"{"theme":"light","port":8080}"#, 0, 10);
        let b2 = CommandBlock::new_completed("api", "pwsh", ".", r#"{"theme":"dark","port":8080,"debug":true}"#, 0, 10);

        let report = compare_blocks_structured(&b1, &b2);
        match report.kind {
            StructuredDiffKind::Json { added_paths, modified_paths, .. } => {
                assert!(added_paths.contains(&"debug".to_string()));
                assert!(modified_paths.iter().any(|m| m.path == "theme"));
            }
            _ => panic!("Expected JSON diff"),
        }
    }

    #[test]
    fn test_compare_process_snapshots_delta() {
        let headers: Vec<String> = crate::process_inspector::PROCESS_TABLE_HEADERS
            .iter()
            .map(|h| h.to_string())
            .collect();
        let row = |pid: &str, name: &str, mem: &str| -> Vec<String> {
            vec![pid.into(), "1".into(), name.into(), "0.0".into(), mem.into(), "dev".into(), "Run".into(), "".into(), name.into()]
        };
        let b1 = CommandBlock::new_structured(
            ":ps",
            "mrt-proc",
            ".",
            String::new(),
            StructuredOutput::Table {
                headers: headers.clone(),
                rows: vec![row("10", "node", "100.0 MB"), row("11", "vite", "50.0 MB")],
            },
            0,
            5,
        );
        let b2 = CommandBlock::new_structured(
            ":ps",
            "mrt-proc",
            ".",
            String::new(),
            StructuredOutput::Table {
                headers,
                rows: vec![row("10", "node", "400.0 MB"), row("12", "cargo", "20.0 MB")],
            },
            0,
            5,
        );
        match compare_blocks_structured(&b1, &b2).kind {
            StructuredDiffKind::ProcessesDelta { started, exited, memory_grown, .. } => {
                assert!(started[0].contains("cargo"));
                assert!(exited[0].contains("vite"));
                assert!(memory_grown[0].contains("node"));
            }
            other => panic!("Expected ProcessesDelta, got {other:?}"),
        }
    }

    #[test]
    fn test_compare_ports_delta() {
        let b1 = CommandBlock::new_structured(
            ":ports",
            "mrt-net",
            ".",
            "".to_string(),
            StructuredOutput::Table {
                headers: vec!["Proto".into(), "Local".into(), "Port".into(), "State".into(), "PID".into()],
                rows: vec![
                    vec!["TCP".into(), "0.0.0.0".into(), "80".into(), "LISTEN".into(), "100".into()],
                ],
            },
            0,
            10,
        );

        let b2 = CommandBlock::new_structured(
            ":ports",
            "mrt-net",
            ".",
            "".to_string(),
            StructuredOutput::Table {
                headers: vec!["Proto".into(), "Local".into(), "Port".into(), "State".into(), "PID".into()],
                rows: vec![
                    vec!["TCP".into(), "0.0.0.0".into(), "80".into(), "LISTEN".into(), "100".into()],
                    vec!["TCP".into(), "0.0.0.0".into(), "443".into(), "LISTEN".into(), "100".into()],
                ],
            },
            0,
            10,
        );

        let report = compare_blocks_structured(&b1, &b2);
        match report.kind {
            StructuredDiffKind::PortsDelta { added_listeners, removed_listeners, unchanged_count } => {
                assert_eq!(unchanged_count, 1);
                assert_eq!(added_listeners.len(), 1);
                assert!(added_listeners[0].contains("443"));
                assert!(removed_listeners.is_empty());
            }
            _ => panic!("Expected PortsDelta"),
        }
    }
}
