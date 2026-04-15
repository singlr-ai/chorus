use anyhow::{Context, Result, anyhow, bail};
use serde_yaml::{Mapping, Sequence, Value};
use sing_bridge::{BoardSpecRecord, SpecBoardSummary, SpecCounts, SpecRecord, SpecStatus};

use crate::types::{OptionalValue, SpecMetadataPatch};

const MAX_SPEC_ID_LEN: usize = 80;

#[derive(Debug, Clone)]
pub struct SpecIndexDocument {
    root: Mapping,
}

impl SpecIndexDocument {
    pub fn parse(content: Option<&str>) -> Result<Self> {
        let value = match content {
            Some(content) if !content.trim().is_empty() => {
                serde_yaml::from_str::<Value>(content).context("failed to parse spec index yaml")?
            }
            _ => Value::Mapping(Mapping::new()),
        };

        let mut root = match value {
            Value::Mapping(mapping) => mapping,
            _ => bail!("spec index must contain a top-level mapping"),
        };

        match root.get(&yaml_key("specs")) {
            None => {
                root.insert(yaml_key("specs"), Value::Sequence(Vec::new()));
            }
            Some(Value::Sequence(_)) => {}
            Some(_) => bail!("spec index field `specs` must be a sequence"),
        }

        Ok(Self { root })
    }

    pub fn spec_records(&self) -> Result<Vec<SpecRecord>> {
        self.spec_values()
            .iter()
            .enumerate()
            .map(|(index, value)| parse_spec_record(value, index))
            .collect()
    }

    pub fn update_spec(&mut self, spec_id: &str, patch: &SpecMetadataPatch) -> Result<SpecRecord> {
        validate_spec_id(spec_id)?;
        let specs = self.spec_values_mut()?;
        let mut entry = None;
        for value in specs.iter_mut() {
            if spec_id_from_value(value)? == spec_id {
                entry = Some(value);
                break;
            }
        }

        let entry = entry.ok_or_else(|| anyhow!("spec `{spec_id}` not found in index.yaml"))?;
        let mapping = entry
            .as_mapping_mut()
            .ok_or_else(|| anyhow!("spec `{spec_id}` is not a mapping"))?;

        if let Some(title) = &patch.title {
            set_string(mapping, "title", normalize_required_string("title", title)?);
        }
        if let Some(status) = patch.status {
            set_string(mapping, "status", status.as_cli_arg().to_string());
        }
        if let Some(assignee) = &patch.assignee {
            apply_optional_string(mapping, "assignee", assignee)?;
        }
        if let Some(branch) = &patch.branch {
            apply_optional_branch(mapping, branch)?;
        }
        if let Some(depends_on) = &patch.depends_on {
            apply_depends_on(mapping, spec_id, depends_on)?;
        }

        parse_spec_record(&Value::Mapping(mapping.clone()), 0)
    }

    pub fn render(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(&Value::Mapping(self.root.clone()))
            .context("failed to serialize spec index yaml")?;
        Ok(yaml.strip_prefix("---\n").unwrap_or(&yaml).to_string())
    }

    fn spec_values(&self) -> &Sequence {
        self.root
            .get(&yaml_key("specs"))
            .and_then(Value::as_sequence)
            .expect("specs sequence must exist after parsing")
    }

    fn spec_values_mut(&mut self) -> Result<&mut Sequence> {
        self.root
            .get_mut(&yaml_key("specs"))
            .and_then(Value::as_sequence_mut)
            .ok_or_else(|| anyhow!("spec index field `specs` must be a sequence"))
    }
}

pub fn compute_board(specs: &[SpecRecord]) -> (Vec<BoardSpecRecord>, SpecBoardSummary) {
    let done = specs
        .iter()
        .filter(|spec| spec.status == SpecStatus::Done)
        .map(|spec| spec.id.as_str())
        .collect::<std::collections::HashSet<_>>();

    let mut counts = SpecCounts::default();
    let mut ready_count = 0;
    let mut blocked_count = 0;
    let mut next_ready_id = None;
    let mut entries = Vec::with_capacity(specs.len());

    for spec in specs {
        match spec.status {
            SpecStatus::Pending => counts.pending += 1,
            SpecStatus::InProgress => counts.in_progress += 1,
            SpecStatus::Review => counts.review += 1,
            SpecStatus::Done => counts.done += 1,
        }

        let unmet_dependencies = spec
            .depends_on
            .iter()
            .filter(|dependency| !done.contains(dependency.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let ready = spec.status == SpecStatus::Pending && unmet_dependencies.is_empty();
        let blocked = spec.status == SpecStatus::Pending && !spec.depends_on.is_empty() && !ready;

        if ready {
            ready_count += 1;
            if next_ready_id.is_none() {
                next_ready_id = Some(spec.id.clone());
            }
        } else if blocked {
            blocked_count += 1;
        }

        entries.push(BoardSpecRecord {
            spec: spec.clone(),
            ready,
            blocked,
            unmet_dependencies: if blocked {
                unmet_dependencies
            } else {
                Vec::new()
            },
        });
    }

    (
        entries,
        SpecBoardSummary {
            counts,
            ready_count,
            blocked_count,
            next_ready_id,
        },
    )
}

fn parse_spec_record(value: &Value, index: usize) -> Result<SpecRecord> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow!("spec entry {} is not a mapping", index + 1))?;
    let id = required_string(mapping, "id")?;
    validate_spec_id(&id)?;

    let title = optional_string(mapping, "title")?.unwrap_or_default();
    let status = optional_string(mapping, "status")?
        .map(|status| parse_status(&status))
        .transpose()?
        .unwrap_or_default();
    let assignee = optional_string(mapping, "assignee")?;
    let depends_on = optional_string_list(mapping, "depends_on")?;
    let branch = optional_string(mapping, "branch")?;

    for dependency in &depends_on {
        validate_spec_id(dependency)?;
    }
    if let Some(branch) = &branch {
        validate_branch(branch)?;
    }

    Ok(SpecRecord {
        id,
        title,
        status,
        assignee,
        depends_on,
        branch,
    })
}

fn required_string(mapping: &Mapping, key: &str) -> Result<String> {
    let value = optional_string(mapping, key)?
        .ok_or_else(|| anyhow!("spec entry field `{key}` is required"))?;
    if value.is_empty() {
        bail!("spec entry field `{key}` must not be blank");
    }
    Ok(value)
}

fn optional_string(mapping: &Mapping, key: &str) -> Result<Option<String>> {
    match mapping.get(&yaml_key(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => bail!("spec entry field `{key}` must be a string"),
    }
}

fn optional_string_list(mapping: &Mapping, key: &str) -> Result<Vec<String>> {
    match mapping.get(&yaml_key(key)) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Sequence(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(value) => Ok(value.clone()),
                _ => bail!("spec entry field `{key}` must contain only strings"),
            })
            .collect(),
        Some(_) => bail!("spec entry field `{key}` must be a sequence"),
    }
}

fn spec_id_from_value(value: &Value) -> Result<String> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow!("spec entry is not a mapping"))?;
    required_string(mapping, "id")
}

fn set_string(mapping: &mut Mapping, key: &str, value: String) {
    mapping.insert(yaml_key(key), Value::String(value));
}

fn apply_optional_string(
    mapping: &mut Mapping,
    key: &str,
    value: &OptionalValue<String>,
) -> Result<()> {
    match value {
        OptionalValue::Set(value) => {
            set_string(mapping, key, normalize_required_string(key, value)?);
        }
        OptionalValue::Clear => {
            mapping.remove(&yaml_key(key));
        }
    }
    Ok(())
}

fn apply_optional_branch(mapping: &mut Mapping, value: &OptionalValue<String>) -> Result<()> {
    match value {
        OptionalValue::Set(branch) => {
            let branch = normalize_required_string("branch", branch)?;
            validate_branch(&branch)?;
            set_string(mapping, "branch", branch);
        }
        OptionalValue::Clear => {
            mapping.remove(&yaml_key("branch"));
        }
    }
    Ok(())
}

fn apply_depends_on(mapping: &mut Mapping, spec_id: &str, depends_on: &[String]) -> Result<()> {
    if depends_on.is_empty() {
        mapping.remove(&yaml_key("depends_on"));
        return Ok(());
    }

    let mut values = Vec::with_capacity(depends_on.len());
    for dependency in depends_on {
        validate_spec_id(dependency)?;
        if dependency == spec_id {
            bail!("spec `{spec_id}` cannot depend on itself");
        }
        values.push(Value::String(dependency.clone()));
    }

    mapping.insert(yaml_key("depends_on"), Value::Sequence(values));
    Ok(())
}

fn normalize_required_string(field: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field} must not be blank");
    }
    if trimmed.contains('\0') {
        bail!("{field} must not contain NUL bytes");
    }
    Ok(trimmed.to_string())
}

fn parse_status(value: &str) -> Result<SpecStatus> {
    match value {
        "pending" => Ok(SpecStatus::Pending),
        "in_progress" => Ok(SpecStatus::InProgress),
        "review" => Ok(SpecStatus::Review),
        "done" => Ok(SpecStatus::Done),
        _ => bail!("invalid spec status `{value}`"),
    }
}

fn validate_spec_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_SPEC_ID_LEN
        || !value
            .chars()
            .enumerate()
            .all(|(index, ch)| project_or_spec_char_allowed(index == 0, ch))
    {
        bail!(
            "spec id `{value}` must match [a-z0-9][a-z0-9-]* and be at most {MAX_SPEC_ID_LEN} characters"
        );
    }

    Ok(())
}

fn validate_branch(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains("..")
        || !value
            .chars()
            .enumerate()
            .all(|(index, ch)| git_ref_char_allowed(index == 0, ch))
    {
        bail!("branch `{value}` must match [a-zA-Z0-9][a-zA-Z0-9._/-]* and not contain '..'");
    }

    Ok(())
}

fn project_or_spec_char_allowed(first: bool, ch: char) -> bool {
    if first {
        ch.is_ascii_lowercase() || ch.is_ascii_digit()
    } else {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'
    }
}

fn git_ref_char_allowed(first: bool, ch: char) -> bool {
    if first {
        ch.is_ascii_alphanumeric()
    } else {
        ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '/' | '-')
    }
}

fn yaml_key(key: &str) -> Value {
    Value::String(key.to_string())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_yaml::Value;
    use sing_bridge::SpecStatus;

    use super::{SpecIndexDocument, compute_board};
    use crate::types::{OptionalValue, SpecMetadataPatch};

    #[test]
    fn metadata_updates_preserve_order_and_unknown_fields() {
        let source = r#"
version: 1
specs:
  - id: alpha
    title: Alpha
    status: done
    priority: high
  - id: beta
    title: Beta
    status: pending
    labels:
      - core
footer: keep
"#;

        let mut document = SpecIndexDocument::parse(Some(source)).unwrap();
        let updated = document
            .update_spec(
                "beta",
                &SpecMetadataPatch {
                    title: Some("Beta 2".to_string()),
                    status: Some(SpecStatus::InProgress),
                    branch: Some(OptionalValue::Set("feat/beta".to_string())),
                    ..Default::default()
                },
            )
            .unwrap();
        let rendered = document.render().unwrap();

        assert_eq!(updated.title, "Beta 2");
        assert_eq!(updated.status, SpecStatus::InProgress);

        let yaml = serde_yaml::from_str::<Value>(&rendered).unwrap();
        let specs = yaml
            .get("specs")
            .and_then(Value::as_sequence)
            .unwrap()
            .iter()
            .map(|value| value.get("id").and_then(Value::as_str).unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(specs, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(yaml.get("version").and_then(Value::as_i64), Some(1));
        assert_eq!(yaml.get("footer").and_then(Value::as_str), Some("keep"));
        assert_eq!(yaml["specs"][0]["priority"].as_str(), Some("high"));
        assert_eq!(yaml["specs"][1]["labels"][0].as_str(), Some("core"));
        assert_eq!(yaml["specs"][1]["branch"].as_str(), Some("feat/beta"));
    }

    #[test]
    fn board_summary_matches_sing_dependency_rules() {
        let source = r#"
specs:
  - id: done-a
    title: Done A
    status: done
  - id: ready-b
    title: Ready B
    status: pending
    depends_on: [done-a]
  - id: blocked-c
    title: Blocked C
    status: pending
    depends_on: [ready-b]
  - id: active-d
    title: Active D
    status: in_progress
    depends_on: [done-a]
"#;

        let document = SpecIndexDocument::parse(Some(source)).unwrap();
        let specs = document.spec_records().unwrap();
        let (entries, summary) = compute_board(&specs);

        assert_eq!(summary.counts.pending, 2);
        assert_eq!(summary.counts.in_progress, 1);
        assert_eq!(summary.counts.done, 1);
        assert_eq!(summary.ready_count, 1);
        assert_eq!(summary.blocked_count, 1);
        assert_eq!(summary.next_ready_id.as_deref(), Some("ready-b"));

        let blocked = entries
            .iter()
            .find(|entry| entry.spec.id == "blocked-c")
            .unwrap();
        assert!(blocked.blocked);
        assert_eq!(blocked.unmet_dependencies, vec!["ready-b".to_string()]);
    }
}
