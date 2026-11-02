use serde::Serialize;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntryDto {
    pub name: String,
    pub relative_path: String,
    pub kind: String,
    pub size: u64,
    pub directory_token: Option<String>,
    pub phase: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPageDto {
    pub directory_token: String,
    pub parent_token: Option<String>,
    pub relative_path: String,
    pub entries: Vec<DirectoryEntryDto>,
    pub next_cursor: Option<String>,
}
pub(crate) struct DirectoryFrame {
    pub dir: cap_std::fs::Dir,
    pub entries: cap_std::fs::ReadDir,
    pub relative: String,
    pub had_entry: bool,
    pub layer: Option<crate::ignore_rules::IgnoreLayer>,
}
pub(crate) struct DirectoryWalk {
    pub stack: Vec<DirectoryFrame>,
    pub skipped_symlinks: u64,
    pub skipped_empty_directories: u64,
    pub options: Option<crate::types::IgnoreOptions>,
    pub custom: Option<std::sync::Arc<ignore::gitignore::Gitignore>>,
    pub root_name: String,
}
pub(crate) struct DirectoryFile {
    pub relative: String,
    pub file: std::fs::File,
    pub size: u64,
}
pub(crate) struct BrowseDirectory {
    pub token: String,
    pub dir: cap_std::fs::Dir,
    pub relative: String,
}
pub(crate) struct DirectoryBrowser {
    pub chain: Vec<BrowseDirectory>,
    pub entries: Option<cap_std::fs::ReadDir>,
    pub cursor: Option<String>,
    pub children: Vec<BrowseDirectory>,
}
