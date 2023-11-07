use std::collections::HashMap;
use std::fs::File;

struct FlatFolderMetadata {
    files: HashMap<String, File>,
}

impl FlatFolderMetadata {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    fn add_file(&mut self, file_name: String, file_handler: File) {
        self.files.insert(file_name, file_handler);
    }

    fn remove_file(&mut self, file_name: &str) -> Option<File> {
        self.files.remove(file_name)
    }

    fn get_file(&self, file_name: &str) -> Option<&File> {
        self.files.get(file_name)
    }

    fn get_file_mut(&mut self, file_name: &str) -> Option<&mut File> {
        self.files.get_mut(file_name)
    }
}
