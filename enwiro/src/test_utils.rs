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
        client::{CookbookTrait, Recipe},
        commands::adapter::EnwiroAdapterTrait,
        config::ConfigurationValues,
        context::CommandContext,
        notifier::Notifier,
    };

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

    pub struct EnwiroAdapterMock {
        pub current_environment: String,
        pub activated: AdapterLog,
    }

    impl EnwiroAdapterTrait for EnwiroAdapterMock {
        fn get_active_environment_name(&self) -> anyhow::Result<String> {
            Ok(self.current_environment.to_string())
        }

        fn activate(&self, name: &str) -> anyhow::Result<()> {
            self.activated.borrow_mut().push(name.to_string());
            Ok(())
        }
    }

    impl EnwiroAdapterMock {
        pub fn new(current_environment: &str) -> Self {
            Self {
                current_environment: current_environment.to_string(),
                activated: Rc::new(RefCell::new(vec![])),
            }
        }
    }

    pub struct FakeCookbook {
        pub cookbook_name: String,
        pub recipes: Vec<Recipe>,
        pub cook_results: std::collections::HashMap<String, String>,
    }

    impl FakeCookbook {
        pub fn new(name: &str, recipes: Vec<&str>, cook_results: Vec<(&str, &str)>) -> Self {
            Self {
                cookbook_name: name.to_string(),
                recipes: recipes.into_iter().map(|s| Recipe::new(s)).collect(),
                cook_results: cook_results
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }

        pub fn new_with_descriptions(
            name: &str,
            recipes: Vec<(&str, Option<&str>)>,
            cook_results: Vec<(&str, &str)>,
        ) -> Self {
            Self {
                cookbook_name: name.to_string(),
                recipes: recipes
                    .into_iter()
                    .map(|(n, d)| match d {
                        Some(desc) => Recipe::with_description(n, desc),
                        None => Recipe::new(n),
                    })
                    .collect(),
                cook_results: cook_results
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }
    }

    pub struct FailingCookbook {
        pub cookbook_name: String,
    }

    impl CookbookTrait for FailingCookbook {
        fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
            anyhow::bail!("simulated failure")
        }

        fn cook(&self, _recipe: &str) -> anyhow::Result<String> {
            anyhow::bail!("simulated failure")
        }

        fn name(&self) -> &str {
            &self.cookbook_name
        }
    }

    impl CookbookTrait for FakeCookbook {
        fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
            Ok(self.recipes.clone())
        }

        fn cook(&self, recipe: &str) -> anyhow::Result<String> {
            self.cook_results
                .get(recipe)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Recipe not found: {}", recipe))
        }

        fn name(&self) -> &str {
            &self.cookbook_name
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
            let environment_directory =
                Path::new(&self.config.workspaces_directory).join(environment_name);
            create_dir(environment_directory).expect("Could not create directory");
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
