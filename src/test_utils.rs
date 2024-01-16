#[cfg(test)]
pub mod test_utils {
    use std::{
        fs::create_dir,
        io::{Cursor, Read},
        path::Path,
    };

    use crate::context::CommandContext;

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
}
