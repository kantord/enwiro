#[cfg(test)]
pub mod test_utils {

    use std::{
        env::temp_dir,
        fs::create_dir,
        io::{Cursor, Read},
        path::Path,
    };

    use rand::Rng;
    use rstest::fixture;

    use crate::{
        config::ConfigurationValues,
        context::{CommandContext, EnwiroAdapterTrait},
    };

    pub struct EnwiroAdapterMock {
        pub current_environment: String,
    }

    impl EnwiroAdapterTrait for EnwiroAdapterMock {
        fn get_active_environment_name(&self) -> Result<String, std::io::Error> {
            Ok(self.current_environment.to_string())
        }
    }

    impl EnwiroAdapterMock {
        pub fn new(current_environment: &str) -> Self {
            Self {
                current_environment: current_environment.to_string(),
            }
        }
    }

    pub type FakeIO = Cursor<Vec<u8>>;
    pub type FakeContext = CommandContext<Cursor<Vec<u8>>, Cursor<Vec<u8>>>;

    impl FakeContext {
        pub fn get_output(&mut self) -> String {
            let mut output = String::new();
            self.writer.set_position(0);

            self.writer
                .read_to_string(&mut output)
                .expect("Could not read output");

            return output;
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
    pub fn context_object() -> FakeContext {
        let temporary_directory_path = temp_dir().join(
            rand::thread_rng()
                .gen_range(100000000..999999999)
                .to_string(),
        );
        create_dir(&temporary_directory_path).expect("Could not create temporary directory");
        let reader = in_memory_buffer();
        let writer = in_memory_buffer();
        let mut config = ConfigurationValues::default();
        config.workspaces_directory = temporary_directory_path.to_str().unwrap().to_string();

        return CommandContext {
            config,
            reader,
            writer,
            adapter: Box::new(EnwiroAdapterMock::new("foobaz")),
        };
    }
}
