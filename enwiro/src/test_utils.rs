#[cfg(test)]
pub mod test_utilities {

    use rstest::fixture;
    use std::{
        fs::create_dir,
        io::{Cursor, Read as _},
        path::Path,
    };
    use tempfile::TempDir;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::{
        commands::adapter::EnwiroAdapterTrait, context::CommandContext, notifier::Notifier,
    };
    use enwiro_daemon::ConfigurationValues;
    use enwiro_sdk::client::CachedRecipe;
    pub use enwiro_sdk::test_helpers::{FailingCookbook, FakeCookbook};

    pub type AdapterLog = Rc<RefCell<Vec<String>>>;
    pub type NotificationLog = Rc<RefCell<Vec<String>>>;

    pub struct MockNotifier {
        pub log: NotificationLog,
    }

    impl MockNotifier {
        pub fn new() -> Self {
            Self {
                log: Rc::new(RefCell::new(vec![])),
            }
        }
    }

    impl Notifier for MockNotifier {
        fn notify_success(&self, message: &str) {
            self.log.borrow_mut().push(format!("SUCCESS: {}", message));
        }

        fn notify_error(&self, message: &str) {
            self.log.borrow_mut().push(format!("ERROR: {}", message));
        }
    }

    pub type RunLog = Rc<RefCell<Vec<enwiro_sdk::adapter::RunPayload>>>;

    pub struct EnwiroAdapterMock {
        pub current_environment: String,
        pub activated: AdapterLog,
        pub runs: RunLog,
    }

    impl EnwiroAdapterTrait for EnwiroAdapterMock {
        fn get_active_environment_name(&self) -> anyhow::Result<String> {
            Ok(self.current_environment.to_string())
        }

        fn activate(
            &self,
            name: &str,
            _managed_envs: &[enwiro_sdk::adapter::ManagedEnvInfo],
            _gear: &std::collections::HashMap<String, enwiro_sdk::gear::Gear>,
        ) -> anyhow::Result<()> {
            self.activated.borrow_mut().push(name.to_string());
            Ok(())
        }

        fn run(&self, payload: &enwiro_sdk::adapter::RunPayload) -> anyhow::Result<()> {
            self.runs.borrow_mut().push(payload.clone());
            Ok(())
        }
    }

    impl EnwiroAdapterMock {
        pub fn new(current_environment: &str) -> Self {
            Self {
                current_environment: current_environment.to_string(),
                activated: Rc::new(RefCell::new(vec![])),
                runs: Rc::new(RefCell::new(vec![])),
            }
        }
    }

    pub type FakeIO = Cursor<Vec<u8>>;
    pub type FakeContext = CommandContext<Cursor<Vec<u8>>>;

    impl FakeContext {
        pub fn get_output(&mut self) -> String {
            let mut output = String::new();
            self.writer.set_position(0);

            self.writer
                .read_to_string(&mut output)
                .expect("Could not read output");

            output
        }

        pub fn create_mock_environment(&mut self, environment_name: &str) {
            let env_dir = Path::new(&self.config.workspaces_directory).join(environment_name);
            create_dir(&env_dir).expect("Could not create env directory");
            let target_dir = env_dir.join(".target");
            create_dir(&target_dir).expect("Could not create target directory");
            let inner_symlink = env_dir.join(environment_name);
            std::os::unix::fs::symlink(&target_dir, &inner_symlink)
                .expect("Could not create inner symlink");
        }

        /// Populate the daemon recipe cache with a single entry, overwriting any
        /// previous contents.
        pub fn write_cache_entry(&self, cookbook: &str, name: &str) {
            self.write_cache_entries(&[(cookbook, name, None)]);
        }

        /// Populate the daemon recipe cache with the given entries, in order.
        /// Each entry is `(cookbook, name, optional_description)`. An empty
        /// slice writes an empty cache file (still "fresh" but listing nothing).
        pub fn write_cache_entries(&self, entries: &[(&str, &str, Option<&str>)]) {
            let cache_dir = self
                .cache_dir
                .as_ref()
                .expect("cache_dir must be set by the fixture");
            std::fs::create_dir_all(cache_dir).expect("Could not create cache dir");
            let content: String = entries
                .iter()
                .map(|(cookbook, name, description)| {
                    let entry = CachedRecipe {
                        cookbook: (*cookbook).to_string(),
                        name: (*name).to_string(),
                        description: description.map(|d| d.to_string()),
                        sort_order: 0,
                        scores: None,
                    };
                    let mut line = serde_json::to_string(&entry)
                        .expect("CachedRecipe should always serialise");
                    line.push('\n');
                    line
                })
                .collect();
            std::fs::write(cache_dir.join("recipes.cache"), content)
                .expect("Could not write cache file");
        }
    }

    #[fixture]
    pub fn in_memory_buffer() -> FakeIO {
        Cursor::new(vec![])
    }

    #[fixture]
    pub fn context_object() -> (TempDir, FakeContext, AdapterLog, NotificationLog) {
        let temp_dir = TempDir::new().expect("Could not create temporary directory");
        let writer = in_memory_buffer();
        let mut config = ConfigurationValues::default();
        config.workspaces_directory = temp_dir.path().to_str().unwrap().to_string();

        let mock = EnwiroAdapterMock::new("foobaz");
        let activated = mock.activated.clone();

        let mock_notifier = MockNotifier::new();
        let notifications = mock_notifier.log.clone();

        let context = CommandContext {
            config,
            writer,
            adapter: Box::new(mock),
            notifier: Box::new(mock_notifier),
            cookbooks: vec![],
            cache_dir: Some(temp_dir.path().join("daemon")),
        };
        (temp_dir, context, activated, notifications)
    }
}
