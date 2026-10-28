use cap_std::fs::Dir;

/// An open directory capability; operations never reconstruct an absolute path.
#[derive(Debug)]
pub struct ShareRoot {
    pub(super) directory: Dir,
}
