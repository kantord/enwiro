pub trait Notifier {
    fn notify_success(&self, message: &str);
    fn notify_error(&self, message: &str);
}

pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify_success(&self, message: &str) {
        if let Err(e) = notify_rust::Notification::new()
            .summary("enwiro")
            .body(message)
            .icon("dialog-information")
            .show()
        {
            tracing::warn!("Could not send success notification: {}", e);
        }
    }

    fn notify_error(&self, message: &str) {
        if let Err(e) = notify_rust::Notification::new()
            .summary("enwiro")
            .body(message)
            .icon("dialog-error")
            .show()
        {
            tracing::warn!(original_message = %message, "Could not send desktop notification: {}", e);
        }
    }
}
