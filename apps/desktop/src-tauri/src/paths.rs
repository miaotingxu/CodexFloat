use std::path::{Path, PathBuf};

pub(crate) fn codex_home_from(
    codex_home: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    codex_home
        .or_else(|| user_profile.map(|path| path.join(".codex")))
        .or_else(|| home.map(|path| path.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub(crate) fn codex_home() -> PathBuf {
    codex_home_from(
        std::env::var_os("CODEX_HOME").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

pub(crate) fn sessions_path_from(
    codex_home: Option<&Path>,
    user_profile: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    codex_home_from(
        codex_home.map(Path::to_path_buf),
        user_profile.map(Path::to_path_buf),
        home.map(Path::to_path_buf),
    )
    .join("sessions")
}

pub(crate) fn sessions_path() -> PathBuf {
    let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let user_profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    sessions_path_from(
        codex_home.as_deref(),
        user_profile.as_deref(),
        home.as_deref(),
    )
}

pub(crate) fn auth_file_path() -> PathBuf {
    codex_home().join("auth.json")
}

#[cfg(test)]
mod tests {
    use super::{codex_home_from, sessions_path_from};
    use std::path::{Path, PathBuf};

    #[test]
    fn sessions_path_prefers_custom_codex_home() {
        let codex_home = PathBuf::from("D:/custom-codex");
        let user_profile = PathBuf::from("D:/users/tester");
        let home = PathBuf::from("D:/home/tester");
        assert_eq!(
            sessions_path_from(Some(&codex_home), Some(&user_profile), Some(&home)),
            codex_home.join("sessions")
        );
    }

    #[test]
    fn explicit_codex_home_controls_session_and_auth_roots() {
        let root = Path::new("C:/CodexHome");
        assert_eq!(
            sessions_path_from(Some(root), None, None),
            PathBuf::from("C:/CodexHome/sessions")
        );
        assert_eq!(
            codex_home_from(Some(root.to_path_buf()), None, None),
            PathBuf::from("C:/CodexHome")
        );
    }
}
