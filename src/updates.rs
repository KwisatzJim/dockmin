use crate::db::Job;
use serde::Serialize;

#[derive(Serialize)]
pub struct UpdateSummary {
    pub state: &'static str,
    pub checked: Option<i64>,
    pub job_id: Option<String>,
    pub output: String,
}

/// Jobs arrive newest first. A newer mutation or failed check invalidates old
/// results so a stack cannot remain eligible after its images have changed.
pub fn for_stack(jobs: &[Job], stack_id: &str) -> UpdateSummary {
    let latest = jobs.iter().find(|job| {
        job.stack_id.as_deref() == Some(stack_id)
            && matches!(
                job.action.as_str(),
                "check-updates"
                    | "update"
                    | "start"
                    | "stop"
                    | "restart"
                    | "configure"
                    | "compose-save"
                    | "deploy"
                    | "rollback"
                    | "restore"
            )
    });
    let Some(job) = latest else {
        return UpdateSummary {
            state: "unchecked",
            checked: None,
            job_id: None,
            output: String::new(),
        };
    };
    let state = if job.action != "check-updates" {
        if job.action == "update" && job.finished.is_none() {
            "updating"
        } else {
            "unchecked"
        }
    } else if job.finished.is_none() {
        if job.state == "queued" {
            "queued"
        } else {
            "checking"
        }
    } else if job.state != "succeeded" {
        "failed"
    } else if job
        .output
        .lines()
        .any(|line| line.contains(": update available — "))
    {
        "available"
    } else if !job.output.trim().is_empty()
        && job
            .output
            .lines()
            .all(|line| line.contains(": up to date — "))
    {
        "current"
    } else {
        "unknown"
    };
    UpdateSummary {
        state,
        checked: if job.action == "check-updates" {
            job.finished
        } else {
            None
        },
        job_id: Some(job.id.clone()),
        output: if job.action == "check-updates" && job.finished.is_some() {
            job.output.chars().take(2000).collect()
        } else {
            String::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(action: &str, state: &str, output: &str) -> Job {
        Job {
            id: "job".into(),
            host_id: "host".into(),
            stack_id: Some("stack".into()),
            action: action.into(),
            state: state.into(),
            created: 1,
            finished: if matches!(state, "queued" | "running") {
                None
            } else {
                Some(2)
            },
            output: output.into(),
        }
    }
    #[test]
    fn mixed_results_and_no_images_are_not_falsely_current() {
        for (output, expected) in [
            (
                "a: up to date — same\nb: update available — new",
                "available",
            ),
            (
                "a: up to date — same\nb: unknown — registry unavailable",
                "unknown",
            ),
            ("No running containers found", "unknown"),
            ("a: up to date — same", "current"),
        ] {
            assert_eq!(
                for_stack(&[job("check-updates", "succeeded", output)], "stack").state,
                expected
            );
        }
    }
    #[test]
    fn latest_check_or_mutation_supersedes_old_updates() {
        let available = job("check-updates", "succeeded", "a: update available — new");
        for (new, expected) in [
            (job("check-updates", "queued", ""), "queued"),
            (job("check-updates", "running", ""), "checking"),
            (job("check-updates", "failed", "network error"), "failed"),
            (job("update", "queued", ""), "updating"),
            (job("update", "succeeded", ""), "unchecked"),
            (job("restart", "succeeded", ""), "unchecked"),
        ] {
            assert_eq!(
                for_stack(&[new, available.clone()], "stack").state,
                expected
            );
        }
        assert_eq!(for_stack(&[available], "other-stack").state, "unchecked");
    }
}
