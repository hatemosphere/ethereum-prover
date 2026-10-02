pub(crate) fn extract_panic_message(err: tokio::task::JoinError) -> String {
    if err.is_panic() {
        let panic = err.into_panic();
        if let Some(s) = panic.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = panic.downcast_ref::<&str>() {
            s.to_string()
        } else {
            "Unknown panic".to_string()
        }
    } else {
        "Task was cancelled".to_string()
    }
}

/// Writes `bytes` to `path` so that readers see either the old file or the complete new one:
/// the data goes to a temporary file in the same directory, is synced, and is renamed over
/// `path`.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}
