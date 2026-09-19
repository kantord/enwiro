//! Human-readable rendering of an environment's `Status`, shared by every
//! command that lists environments (`ls`, `stale`, ...) so they render the
//! same label/color for the same status.

use console::style;
use enwiro_daemon::meta::{CookedPhase, Status};

pub fn status_label(status: Option<&Status>) -> &'static str {
    match status {
        Some(Status::Cooked {
            phase: Some(CookedPhase::Active),
            ..
        }) => "active",
        Some(Status::Cooked {
            phase: Some(CookedPhase::Waiting),
            ..
        }) => "waiting",
        Some(Status::Cooked { phase: None, .. }) => "ready",
        Some(Status::Done { .. }) => "done",
        Some(Status::Evergreen) => "evergreen",
        Some(Status::Uncooked) | None => "-",
    }
}

pub fn colorize_status(label: &str) -> String {
    match label {
        "active" => style(label).green().to_string(),
        "waiting" => style(label).yellow().to_string(),
        "ready" => style(label).cyan().to_string(),
        "done" => style(label).dim().to_string(),
        "evergreen" => style(label).blue().to_string(),
        _ => style(label).dim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_daemon::meta::{DoneOutcome, StatusDetail};

    #[test]
    fn label_for_each_status_variant() {
        assert_eq!(status_label(None), "-");
        assert_eq!(status_label(Some(&Status::Uncooked)), "-");
        assert_eq!(
            status_label(Some(&Status::Cooked {
                phase: None,
                detail: None
            })),
            "ready"
        );
        assert_eq!(
            status_label(Some(&Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None
            })),
            "active"
        );
        assert_eq!(
            status_label(Some(&Status::Cooked {
                phase: Some(CookedPhase::Waiting),
                detail: Some(StatusDetail {
                    source: "test".into(),
                    label: "testing".into(),
                    info: None
                })
            })),
            "waiting"
        );
        assert_eq!(
            status_label(Some(&Status::Done {
                outcome: Some(DoneOutcome::Completed)
            })),
            "done"
        );
        assert_eq!(status_label(Some(&Status::Evergreen)), "evergreen");
    }
}
