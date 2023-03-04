#[cfg(feature = "git2")]
mod git2;

#[cfg(feature = "git2")]
pub use self::git2::*;

#[cfg(feature = "git_cli")]
mod git_cli;

#[cfg(feature = "git_cli")]
pub use self::git_cli::*;

#[cfg(test)]
mod tests {
    use std::fs;

    use self_update::TempDir;

    use super::*;

    #[test]
    fn should_retrieve_remove_git_repository() {
        let _ = tracing_subscriber::fmt::try_init();

        let tmp = TempDir::new().unwrap();
        let mut dst = tmp.into_path();
        let url = "https://github.com/ilaborie/ffizer-templates.git";
        let rev = "master";
        dst.push(rev);

        retrieve(&dst, url, rev).unwrap();

        let content = fs::read_dir(&dst).unwrap();
        let mut count = 0;
        for entry in content {
            let entry = entry.unwrap();
            println!("{entry:?}");
            count += 1;
        }
        assert!(count > 0, "Expected a non-empty dst folder");
    }

    #[test]
    #[ignore = "Only works on my laptop"]
    fn should_get_merge_cmd() {
        let result = find_cmd_tool("merge").unwrap();
        assert_eq!(result, "code --wait $MERGED");
    }
}
