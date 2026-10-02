/// A label map, sorted for stable output.
pub type Labels = std::collections::BTreeMap<String, String>;

/// A device of the account, by its name (the hardware serial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    /// `None` when not asked for (listing names only).
    pub labels: Option<Labels>,
}
