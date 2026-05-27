fn notification_id_for_env(env_name: &str) -> u32 {
    let mut hash: u32 = 5381;
    for byte in env_name.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u32::from(byte));
    }
    hash | 1
}

pub trait Notifier {
    fn notify_info(&self, env_name: &str, message: &str);
    fn notify_success(&self, env_name: &str, message: &str);
    fn notify_error(&self, message: &str);
}

pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify_info(&self, env_name: &str, message: &str) {
        if let Err(e) = notify_rust::Notification::new()
            .summary("enwiro")
            .body(message)
            .icon("dialog-information")
            .id(notification_id_for_env(env_name))
            .show()
        {
            tracing::warn!("Could not send info notification: {}", e);
        }
    }

    fn notify_success(&self, env_name: &str, message: &str) {
        if let Err(e) = notify_rust::Notification::new()
            .summary("enwiro")
            .body(message)
            .icon("dialog-information")
            .id(notification_id_for_env(env_name))
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
