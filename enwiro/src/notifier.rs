pub trait Notifier {
    fn notify_success(&self, message: &str);
    fn notify_error(&self, message: &str);
}

pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify_success(&self, message: &str) {
        let _ = notify_rust::Notification::new()
            .summary("enwiro")
            .body(message)
            .icon("dialog-information")
            .show();
    }

    fn notify_error(&self, message: &str) {
        if let Err(e) = notify_rust::Notification::new()
            .summary("enwiro")
            .body(message)
            .icon("dialog-error")
            .show()
        {
            eprintln!("Warning: could not send notification: {}", e);
            eprintln!("{}", message);
        }
    }
}
