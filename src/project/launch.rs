//! What a Project session records on its launch row
//! (`launches.metadata.project`), so every session of a work item is found
//! again as one trace (`tracing::project_report`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The work a session was started for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectLaunch {
    /// The hub, `owner/repo`.
    pub hub: String,
    pub item: u64,
    /// The item's type, as spec-wave names it (`Story`, `Bug`, …).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    /// The Feature above the item, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// The step: `spec`, `plan`, `bug`, `critique`, `decompose`, `build`,
    /// `fix`, `fix-pr`.
    pub step: String,
    #[serde(default = "one")]
    pub attempt: u32,
    /// `work`, `review` or `critique`.
    #[serde(default)]
    pub role: String,
    /// Who ran it: `operator` for a step started by hand.
    #[serde(default)]
    pub executor: String,
    /// The repository the code was written in, when not the hub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_repo: Option<String>,
    /// The workflow run the session belongs to.
    #[serde(default)]
    pub run: String,
}

fn one() -> u32 {
    1
}

impl ProjectLaunch {
    /// This launch as one session of a run: its role is read from the
    /// step's label (a `revise` step's review is `<step>/review`).
    pub fn for_session(&self, run: &str, label: &str) -> ProjectLaunch {
        let mut out = self.clone();
        out.run = run.to_string();
        out.role = if self.step == "critique" {
            "critique"
        } else if label.contains("/review") {
            "review"
        } else {
            "work"
        }
        .to_string();
        out
    }

    /// The value of `launches.metadata.project`.
    pub fn metadata(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Read back from a launch row's metadata.
    pub fn from_metadata(meta: &Value) -> Option<ProjectLaunch> {
        serde_json::from_value(meta.get("project")?.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_launch_is_written_and_read_back_with_the_specs_keys() {
        let l = ProjectLaunch {
            hub: "acme/hub".into(),
            item: 412,
            item_type: Some("Story".into()),
            feature: Some(360),
            milestone: Some("v0.6".into()),
            step: "build".into(),
            attempt: 1,
            executor: "operator".into(),
            code_repo: Some("acme/api".into()),
            ..Default::default()
        }
        .for_session("run-1", "build/review");
        assert_eq!(
            l.metadata(),
            json!({"hub": "acme/hub", "item": 412, "type": "Story", "feature": 360,
                   "milestone": "v0.6", "step": "build", "attempt": 1, "role": "review",
                   "executor": "operator", "code_repo": "acme/api", "run": "run-1"})
        );
        assert_eq!(
            ProjectLaunch::from_metadata(&json!({"project": l.metadata()})),
            Some(l)
        );
        assert_eq!(
            ProjectLaunch::from_metadata(&json!({"workflow": "x"})),
            None
        );
    }

    #[test]
    fn the_role_follows_the_step_and_the_label() {
        let base = ProjectLaunch {
            step: "build".into(),
            ..Default::default()
        };
        assert_eq!(base.for_session("r", "build").role, "work");
        assert_eq!(base.for_session("r", "build/review").role, "review");
        let critique = ProjectLaunch {
            step: "critique".into(),
            ..Default::default()
        };
        assert_eq!(critique.for_session("r", "work").role, "critique");
    }
}
