use serde_json::Value;

/// Compare the running image's repository digests against the registry index
/// and its matching platform manifest. Both are valid RepoDigest forms.
pub fn compare(local: &Value, remote: &Value) -> (&'static str, &'static str) {
    let digests = local["RepoDigests"].as_array();
    let Some(digests) = digests.filter(|v| !v.is_empty()) else {
        return (
            "unknown",
            "Running image has no repository digest (possibly locally built)",
        );
    };
    let mut candidates = vec![];
    if let Some(d) = remote["digest"].as_str() {
        candidates.push(d)
    }
    let os = local["Os"].as_str().unwrap_or_default();
    let arch = local["Architecture"].as_str().unwrap_or_default();
    let variant = local["Variant"].as_str().unwrap_or_default();
    if let Some(manifests) = remote["manifests"].as_array() {
        let platform: Vec<_> = manifests
            .iter()
            .filter(|m| {
                m["platform"]["os"].as_str() == Some(os)
                    && m["platform"]["architecture"].as_str() == Some(arch)
                    && m["platform"]["variant"].as_str().unwrap_or_default() == variant
            })
            .collect();
        if platform.is_empty() {
            return (
                "unknown",
                "Registry does not report a matching image platform",
            );
        }
        for manifest in platform {
            if let Some(d) = manifest["digest"].as_str() {
                candidates.push(d)
            }
        }
    }
    if candidates.is_empty() {
        return (
            "unknown",
            "Registry manifest did not contain a comparable digest",
        );
    }
    if digests.iter().filter_map(Value::as_str).any(|d| {
        d.rsplit_once('@')
            .is_some_and(|(_, hash)| candidates.contains(&hash))
    }) {
        (
            "up to date",
            "Running image matches the current registry digest",
        )
    } else {
        (
            "update available",
            "The current tag points to a different image digest",
        )
    }
}
pub fn summarize(output: &str) -> String {
    let mut result = Vec::new();
    for block in output.split("DOCKMIN_IMAGE ").skip(1) {
        let Some((image, body)) = block.split_once('\n') else {
            continue;
        };
        let inspected = body
            .strip_prefix("LOCAL\n")
            .and_then(|b| b.split_once("\nREMOTE\n"))
            .and_then(|(local, remote)| {
                remote
                    .split_once("\nEND_IMAGE")
                    .map(|(remote, _)| (local, remote))
            });
        let (state, reason) = match inspected {
            Some((local, remote)) => match (
                serde_json::from_str::<Value>(local),
                serde_json::from_str::<Value>(remote),
            ) {
                (Ok(l), Ok(r)) => compare(&l, &r),
                _ => (
                    "unknown",
                    "Image inspection failed; check buildx, registry access, or authentication",
                ),
            },
            None => ("unknown", "Incomplete image inspection output"),
        };
        result.push(format!("{image}: {state} — {reason}"));
    }
    if result.is_empty() {
        "No running containers found, or image inspection could not complete.".into()
    } else {
        result.join("\n")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn index_and_platform_digests() {
        let mut local =
            json!({"Os":"linux","Architecture":"amd64","RepoDigests":["repo@sha256:platform"]});
        let remote = json!({"digest":"sha256:index","manifests":[{"digest":"sha256:platform","platform":{"os":"linux","architecture":"amd64"}}]});
        assert_eq!(compare(&local, &remote).0, "up to date");
        local["RepoDigests"] = json!(["repo@sha256:index"]);
        assert_eq!(compare(&local, &remote).0, "up to date");
        local["RepoDigests"] = json!(["repo@sha256:old"]);
        assert_eq!(compare(&local, &remote).0, "update available");
        local["Architecture"] = json!("arm64");
        assert_eq!(compare(&local, &remote).0, "unknown");
    }
    #[test]
    fn multiline_registry_output() {
        let output = "DOCKMIN_IMAGE nginx:latest\nLOCAL\n{\"RepoDigests\":[\"nginx@sha256:current\"]}\nREMOTE\n{\n  \"digest\": \"sha256:current\"\n}\nEND_IMAGE\n";
        assert!(summarize(output).contains("up to date"));
        assert!(summarize("DOCKMIN_IMAGE app\nLOCAL\n{}").contains("unknown"));
    }
    #[test]
    fn local_and_missing_manifest_are_unknown() {
        assert_eq!(compare(&json!({}), &json!({})).0, "unknown");
        assert_eq!(
            compare(&json!({"RepoDigests":["repo@sha256:a"]}), &json!({})).0,
            "unknown"
        );
    }
}
