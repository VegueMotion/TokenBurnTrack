use std::path::PathBuf;

pub const DATA_DIR_ENV: &str = "TBT_DATA_DIR";
pub const DB_FILE: &str = "tbt.sqlite";

pub fn default_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "tokenburntrack").map(|d| d.data_dir().to_path_buf())
}

pub fn db_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(DB_FILE)
}
