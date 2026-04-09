//! Custom Attribute diff types.
//!
//! The only mutation `apply` can perform is the deprecation flag toggle.

#[derive(Debug, Clone)]
pub struct CustomAttributeDiff {
    pub name: String,
    pub op: CustomAttributeOp,
}

#[derive(Debug, Clone)]
pub enum CustomAttributeOp {
    /// Present in Braze but missing from local registry. Action: prompt `export`.
    UnregisteredInGit,
    /// Present in local registry but not in Braze. Often a typo.
    PresentInGitOnly,
    /// `deprecated` flag changed. The only mutation `apply` actually performs.
    DeprecationToggled {
        from: bool,
        to: bool,
    },
    /// Only the description changed. No API to update it, so `apply` is a no-op.
    MetadataOnly,
    Unchanged,
}

impl CustomAttributeDiff {
    pub fn has_changes(&self) -> bool {
        !matches!(self.op, CustomAttributeOp::Unchanged)
    }
}
