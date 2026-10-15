//! Docker volumes are reviewed as exact objects. Broad prune would widen the
//! deletion set when stopped containers disappear between review and execution.

use super::*;

pub(super) fn inspect(
    host: &dyn ProviderHost,
    identity: &ProviderIdentity,
    name: &str,
    selected: &HashSet<String>,
) -> Result<Vec<DetectedTarget>, ProviderError> {
    let executable = identity.executable.as_ref().expect("Docker executable");
    let listing = successful_stdout(
        host,
        &plain_inspection_command(
            executable,
            &[
                "--context",
                name,
                "volume",
                "ls",
                "--quiet",
                "--filter",
                "dangling=true",
            ],
        ),
    )?;
    let names: Vec<_> = listing
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if names.len() > 128 {
        return Err(ProviderError::new(
            "Docker volume inspection exceeded 128 objects; refine inventory before cleanup",
        ));
    }
    let sizes = successful_stdout(
        host,
        &plain_inspection_command(
            executable,
            &[
                "--context",
                name,
                "system",
                "df",
                "--verbose",
                "--format",
                "{{json .Volumes}}",
            ],
        ),
    )
    .ok()
    .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
    .unwrap_or_default()
    .into_iter()
    .filter_map(|value| {
        Some((
            value["Name"].as_str()?.to_owned(),
            parse_human_bytes(value["Size"].as_str()?)?,
        ))
    })
    .collect::<BTreeMap<_, _>>();
    let mut items = Vec::new();
    for volume in names {
        if !valid_name(volume) {
            return Err(ProviderError::new("Docker returned an invalid volume name"));
        }
        let details = successful_stdout(
            host,
            &plain_inspection_command(
                executable,
                &["--context", name, "volume", "inspect", volume],
            ),
        )?;
        let values: Vec<serde_json::Value> = serde_json::from_str(&details)
            .map_err(|e| ProviderError::new(format!("invalid Docker volume JSON: {e}")))?;
        let value = values
            .first()
            .filter(|v| v["Name"].as_str() == Some(volume))
            .ok_or_else(|| ProviderError::new("Docker volume identity did not match inspection"))?;
        let anonymous = value["Labels"]
            .as_object()
            .is_some_and(|labels| labels.contains_key("com.docker.volume.anonymous"));
        let id = if anonymous {
            "docker.volumes-anonymous"
        } else {
            "docker.volumes-named"
        };
        if !selected.contains(id) {
            continue;
        }
        let references = successful_stdout(
            host,
            &plain_inspection_command(
                executable,
                &[
                    "--context",
                    name,
                    "container",
                    "ls",
                    "--all",
                    "--quiet",
                    "--filter",
                    &format!("volume={volume}"),
                ],
            ),
        )?;
        let local = value["Driver"].as_str() == Some("local")
            && value["Options"].as_object().is_none_or(|o| o.is_empty());
        let created = value["CreatedAt"].as_str().filter(|s| !s.is_empty());
        let eligible = references.trim().is_empty() && local && created.is_some();
        let mut provider = identity.clone();
        provider.context = Some(format!(
            "{}; volume {volume}; created {}",
            identity.context.as_deref().unwrap_or(name),
            created.unwrap_or("unknown")
        ));
        let operation = if eligible {
            PlannedOperation::Command {
                command: CommandSpec {
                    executable: executable.clone(),
                    args: ["--context", name, "volume", "rm", "--", volume]
                        .map(str::to_owned)
                        .to_vec(),
                    environment: BTreeMap::new(),
                },
            }
        } else {
            PlannedOperation::Informational
        };
        let mut item = provider_item(id, &provider, &format!("remove unused {} Docker volume {volume}", if anonymous { "anonymous" } else { "named" }),
            if anonymous { TargetClass::Network } else { TargetClass::Irreplaceable },
            CleanupLevel::Standard, UsageEstimate::Unknown { explanation: "cleanup yield unknown: volume inspect has no trustworthy byte total; engine bytes differ from host recovery".into() },
            if eligible { CandidateSafety::Eligible } else { CandidateSafety::Refused("volume is referenced, uses external storage, or has no creation identity".into()) },
            operation, None, 0,
            "PERMANENT DATA LOSS: this exact volume is removed; newly unused volumes require fresh review. Docker rejects removal if a container acquires a reference.",
            vec![format!("reviewed volume name: {volume}"), "checked references from running AND stopped containers; no force or broad prune".into()]);
        if let Some(bytes) = sizes.get(volume) {
            item.usage = UsageEstimate::ProviderReported {
                bytes: Some(*bytes), confidence: Confidence::Low,
                explanation: "Docker reports occupied bytes for this unused volume; VM-backed host recovery can differ".into(),
            };
        }
        item.properties.automation = if anonymous {
            Automation::Confirmed
        } else {
            Automation::ExplicitTargetOnly
        };
        items.push(item);
    }
    Ok(items)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::{PlanSelection, plan_cleanup};
    struct Host {
        referenced: bool,
        recreated: bool,
    }
    impl ProviderHost for Host {
        fn resolve_executable(&self, _: &str) -> Result<Option<PathBuf>, ProviderError> {
            Ok(None)
        }
        fn run_inspection(&self, command: &CommandSpec) -> Result<CommandOutput, ProviderError> {
            let tail = command.args[2..]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let stdout = match tail.as_slice() {
                ["volume", "ls", "--quiet", "--filter", "dangling=true"] => "anonymous\nnamed\nexternal".into(),
                ["system", "df", "--verbose", "--format", "{{json .Volumes}}"] => r#"[{"Name":"anonymous","Size":"12MB"},{"Name":"named","Size":"2GB"}]"#.into(),
                ["volume", "inspect", name] => serde_json::json!([{
                    "Name": name, "Driver":"local", "CreatedAt": if self.recreated {"later"} else {"original"},
                    "Labels": if *name == "anonymous" { serde_json::json!({"com.docker.volume.anonymous":""}) } else { serde_json::json!({}) },
                    "Options": if *name == "external" {serde_json::json!({"type":"nfs"})} else {serde_json::json!({})}
                }]).to_string(),
                ["container", "ls", "--all", "--quiet", "--filter", _] => if self.referenced {"stopped-container"} else {""}.into(),
                _ => panic!("unexpected inspection: {:?}", command.args),
            };
            Ok(CommandOutput {
                exit_code: Some(0),
                stdout,
                stderr: String::new(),
            })
        }
    }
    #[test]
    fn volumes_are_exact_context_pinned_and_named_data_requires_individual_review() {
        let provider = ProviderIdentity {
            id: "docker".into(),
            executable: Some("/fixture/docker".into()),
            version: Some("29".into()),
            context: Some("orbstack (unix:///fixture/socket)".into()),
        };
        let selected = HashSet::from([
            "docker.volumes-anonymous".into(),
            "docker.volumes-named".into(),
        ]);
        let items = inspect(
            &Host {
                referenced: false,
                recreated: false,
            },
            &provider,
            "orbstack",
            &selected,
        )
        .unwrap();
        assert_eq!(items.len(), 3);
        for item in &items[..2] {
            let PlannedOperation::Command { command } = &item.operation else {
                panic!()
            };
            assert_eq!(
                &command.args[..5],
                &["--context", "orbstack", "volume", "rm", "--"]
            );
            assert!(
                !command
                    .args
                    .iter()
                    .any(|arg| arg == "--force" || arg == "prune")
            );
        }
        assert!(
            !items[2].manually_selectable(),
            "external local-driver volumes are protected"
        );
        let plan = plan_cleanup(
            items.clone(),
            CleanupLevel::Aggressive,
            &["docker.volumes-named".into()],
            true,
        );
        assert!(matches!(plan[0].selection, PlanSelection::Selected { .. }));
        assert!(matches!(plan[1].selection, PlanSelection::Skipped(_)));
        assert!(matches!(
            items[0].usage,
            UsageEstimate::ProviderReported {
                bytes: Some(12_000_000),
                ..
            }
        ));
        let referenced = inspect(
            &Host {
                referenced: true,
                recreated: false,
            },
            &provider,
            "orbstack",
            &selected,
        )
        .unwrap();
        assert!(referenced.iter().all(|item| !item.manually_selectable()));
        let recreated = inspect(
            &Host {
                referenced: false,
                recreated: true,
            },
            &provider,
            "orbstack",
            &selected,
        )
        .unwrap();
        assert_ne!(
            items[0].provider, recreated[0].provider,
            "same-name replacements must abort"
        );
    }
}
