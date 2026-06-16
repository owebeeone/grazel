#[cfg(test)]
mod tests {
    use crate::state::*;

    #[test]
    fn first_on_path_picks_first_existing_candidate_in_preference_order() {
        // §7 ·iii native cc resolution: candidate order wins within a dir; dirs scanned per candidate.
        let dirs = ["/nope", "/usr/bin", "/usr/local/bin"];
        // Only c++ and clang++ "exist", in different dirs — c++ wins (earlier candidate).
        let present = |p: &std::path::Path| {
            p == std::path::Path::new("/usr/bin/c++")
                || p == std::path::Path::new("/usr/local/bin/clang++")
        };
        assert_eq!(
            first_on_path(&["c++", "clang++"], &dirs, present).as_deref(),
            Some("/usr/bin/c++")
        );
        // Falls through candidates when the preferred one is absent anywhere.
        let only_clang = |p: &std::path::Path| p == std::path::Path::new("/usr/local/bin/clang++");
        assert_eq!(
            first_on_path(&["c++", "clang++"], &dirs, only_clang).as_deref(),
            Some("/usr/local/bin/clang++")
        );
        // None present → None (host_cc then falls back to CXX).
        assert_eq!(first_on_path(&["c++"], &dirs, |_| false), None);
    }
}

