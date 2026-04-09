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
    /// Braze にあるが local registry にない。アクション: `export` を促す。
    UnregisteredInGit,
    /// Local registry にあるが Braze にない。多くは typo。
    PresentInGitOnly,
    /// `deprecated` flag が変わった。`apply` が実際に行う唯一の mutation。
    DeprecationToggled {
        from: bool,
        to: bool,
    },
    /// description だけ変わった。API がないため `apply` は何もしない。
    MetadataOnly,
    Unchanged,
}

impl CustomAttributeDiff {
    pub fn has_changes(&self) -> bool {
        !matches!(self.op, CustomAttributeOp::Unchanged)
    }
}
