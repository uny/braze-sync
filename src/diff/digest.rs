//! Canonical projections of the *remote* side of a resource, hashed for
//! the apply-time precondition check (`apply --plan`).
//!
//! A projection answers one question: *"is the remote object still the one
//! `diff` observed when the plan was written?"* It therefore covers exactly
//! the surface [`syncable_eq`](crate::diff) compares — no more, no less:
//!
//! ```text
//! digest(a) == digest(b)  <=>  syncable_eq(a, b)
//! ```
//!
//! That equivalence is the load-bearing invariant, pinned by tests in this
//! module and in each `diff::*` module. It is what makes the digest and the
//! diff unable to disagree: a remote edit the diff would treat as a real
//! change is a remote edit the digest catches, and a remote representation
//! wobble the diff deliberately absorbs (`None` vs `""`, tag order) is one
//! the digest absorbs too.
//!
//! # What a projection deliberately excludes
//!
//! Read-only fields that braze-sync never writes: `ContentBlock::state`
//! (absent from `/content_blocks/info`), `EmailTemplate::description` and
//! `Catalog::description` (returned by Braze but not settable by
//! create/update). A remote edit to one of those cannot be clobbered by
//! `apply`, so it is not a precondition.
//!
//! # Stability
//!
//! The byte encoding below is part of the plan file format. Changing it
//! invalidates every saved plan, so any change here must bump
//! [`CURRENT_PLAN_VERSION`](crate::diff::plan::CURRENT_PLAN_VERSION).
//! The insta snapshots in this module catch an encoding change *for the
//! three fixtures they pin*, and a failure there is the prompt to bump.
//! Two gaps to keep in mind, because neither is enforced:
//!
//! - Nothing ties the bump to the snapshot. Accept a new one and leave
//!   the version alone, and every already-saved v2 plan starts reporting
//!   drift its remote never had.
//! - An input class the fixtures do not exercise can change encoding
//!   with all three still green — a catalog with a repeated field name
//!   did exactly that when this projection moved to a `BTreeMap`.
//!
//! # Limitation
//!
//! Projections run over domain objects, not the wire bytes. Catalog field
//! types Braze adds after this binary was built collapse to
//! [`CatalogFieldType::Unknown`](crate::resource::CatalogFieldType) at the
//! resource layer, so a remote change between two such types is invisible
//! to the digest. See the type's doc comment.

use blake3::Hasher;
use std::collections::BTreeMap;

use crate::resource::{Catalog, ContentBlock, EmailTemplate};

/// Length-prefixed, domain-separated field accumulator.
///
/// Every value is written as its byte length (u64 LE) followed by its
/// bytes, so no two distinct field sequences can collide by concatenation
/// (`["ab", "c"]` and `["a", "bc"]` hash differently).
struct Projection(Hasher);

impl Projection {
    /// `tag` names the projected type and separates the hash domains, so
    /// a content block and an email template with identical field values
    /// never share a digest.
    fn new(tag: &str) -> Self {
        let mut p = Self(Hasher::new());
        p.str_field(tag);
        p
    }

    fn str_field(&mut self, value: &str) -> &mut Self {
        self.0.update(&(value.len() as u64).to_le_bytes());
        self.0.update(value.as_bytes());
        self
    }

    /// `None` and `Some("")` project identically — mirrors
    /// [`opt_str_eq`](crate::diff::opt_str_eq).
    fn opt_str_field(&mut self, value: &Option<String>) -> &mut Self {
        self.str_field(value.as_deref().unwrap_or(""))
    }

    /// Three-valued: `None` is distinct from `Some(false)`, matching the
    /// plain `==` that `syncable_eq` uses on `should_inline_css`.
    fn opt_bool_field(&mut self, value: Option<bool>) -> &mut Self {
        self.str_field(match value {
            None => "unset",
            Some(true) => "true",
            Some(false) => "false",
        })
    }

    /// Sorted, so tag order does not affect the digest — mirrors
    /// [`tags_eq_unordered`](crate::diff::tags_eq_unordered).
    fn tags_field(&mut self, tags: &[String]) -> &mut Self {
        let mut sorted: Vec<&str> = tags.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        self.str_field(&sorted.len().to_string());
        for tag in sorted {
            self.str_field(tag);
        }
        self
    }

    fn finish(&self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

/// Digest the surface `diff::content_block::syncable_eq` compares.
/// Excludes `state` (read-only: Braze `/info` does not return it).
pub fn content_block(cb: &ContentBlock) -> String {
    let mut p = Projection::new("content_block/v2");
    p.str_field(&cb.name)
        .opt_str_field(&cb.description)
        .str_field(&cb.content)
        .tags_field(&cb.tags)
        .finish()
}

/// Digest the surface `diff::email_template::syncable_eq` compares.
/// Excludes `description` (read-only: create/update cannot set it).
pub fn email_template(tpl: &EmailTemplate) -> String {
    let mut p = Projection::new("email_template/v2");
    p.str_field(&tpl.name)
        .str_field(&tpl.subject)
        .str_field(&tpl.body_html)
        .str_field(&tpl.body_plaintext)
        .opt_str_field(&tpl.preheader)
        .opt_bool_field(tpl.should_inline_css)
        .tags_field(&tpl.tags)
        .finish()
}

/// Digest the surface `diff::catalog::diff_fields` compares: the field set
/// keyed by name, each with its type. Field order does not matter (the
/// diff keys by name via `BTreeMap`), so fields are keyed the same way here.
///
/// Also covers `name`, which `diff_fields` does not look at — the diff is
/// handed two field lists that the caller already paired by catalog name.
/// So the equivalence holds *per catalog name*, which is the only way it
/// is ever used: `diff_ops` pairs ops by `(kind, name, op_type)` before
/// any digest is compared.
///
/// Excludes `description`: Braze has no endpoint to update it, so
/// `diff_schema` treats description-only differences as `Unchanged` and
/// `apply` can never overwrite one.
pub fn catalog(cat: &Catalog) -> String {
    // Keyed by name, exactly as `diff_fields` keys its own `BTreeMap`:
    // that gives the sort for free, and it collapses a repeated field
    // name last-wins the same way the diff does. A sorted `Vec` of pairs
    // would instead distinguish two catalogs the diff cannot tell apart,
    // breaking the equivalence in the direction that matters (digest
    // silent while the diff sees a change) — unreachable through Braze's
    // schema, but the invariant is what everything downstream rests on.
    let fields: BTreeMap<&str, &'static str> = cat
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.field_type.as_str()))
        .collect();

    let mut p = Projection::new("catalog_schema/v2");
    p.str_field(&cat.name);
    p.str_field(&fields.len().to_string());
    for (name, field_type) in fields {
        p.str_field(name).str_field(field_type);
    }
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{CatalogField, CatalogFieldType, ContentBlockState};

    fn cb() -> ContentBlock {
        ContentBlock {
            name: "hero".into(),
            description: Some("banner".into()),
            content: "{{ x }}".into(),
            tags: vec!["b".into(), "a".into()],
            state: ContentBlockState::Active,
        }
    }

    fn tpl() -> EmailTemplate {
        EmailTemplate {
            name: "welcome".into(),
            subject: "Hi".into(),
            body_html: "<p>hi</p>".into(),
            body_plaintext: "hi".into(),
            description: Some("read-only".into()),
            preheader: Some("peek".into()),
            should_inline_css: Some(true),
            tags: vec!["z".into(), "a".into()],
        }
    }

    fn cat() -> Catalog {
        Catalog {
            name: "products".into(),
            description: Some("read-only".into()),
            fields: vec![
                CatalogField {
                    name: "id".into(),
                    field_type: CatalogFieldType::String,
                },
                CatalogField {
                    name: "price".into(),
                    field_type: CatalogFieldType::Number,
                },
            ],
        }
    }

    // -------------------------------------------------------------
    // Snapshots. A failure here means the plan file format changed:
    // bump CURRENT_PLAN_VERSION before accepting the new snapshot.
    // -------------------------------------------------------------

    #[test]
    fn content_block_projection_is_stable() {
        insta::assert_snapshot!(content_block(&cb()));
    }

    #[test]
    fn email_template_projection_is_stable() {
        insta::assert_snapshot!(email_template(&tpl()));
    }

    #[test]
    fn catalog_projection_is_stable() {
        insta::assert_snapshot!(catalog(&cat()));
    }

    // -------------------------------------------------------------
    // Domain separation and encoding unambiguity.
    // -------------------------------------------------------------

    #[test]
    fn digests_are_domain_separated() {
        // Same leading field value, three different projected types.
        let mut a = cb();
        a.name = "same".into();
        a.description = None;
        a.content = String::new();
        a.tags = vec![];

        let mut b = tpl();
        b.name = "same".into();
        b.subject = String::new();
        b.body_html = String::new();
        b.body_plaintext = String::new();
        b.preheader = None;
        b.should_inline_css = None;
        b.tags = vec![];

        let c = Catalog {
            name: "same".into(),
            description: None,
            fields: vec![],
        };

        let (x, y, z) = (content_block(&a), email_template(&b), catalog(&c));
        assert_ne!(x, y);
        assert_ne!(y, z);
        assert_ne!(x, z);
    }

    #[test]
    fn field_boundaries_are_unambiguous() {
        let mut a = cb();
        a.name = "ab".into();
        a.description = Some("c".into());
        let mut b = cb();
        b.name = "a".into();
        b.description = Some("bc".into());
        assert_ne!(content_block(&a), content_block(&b));
    }

    #[test]
    fn tag_count_prefix_prevents_boundary_collision() {
        let mut a = cb();
        a.tags = vec!["x".into(), "y".into()];
        let mut b = cb();
        b.tags = vec!["xy".into()];
        assert_ne!(content_block(&a), content_block(&b));
    }
}
