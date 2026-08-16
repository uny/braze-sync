//! Round-trip invariant for Braze-managed value correlation.
//!
//! Every regression in the #68 / #70 / #73 / #77 family is an instance of
//! one property:
//!
//! > Templatize a body, hand the resolver that template plus a remote body
//! > spelling the same links, and every live managed value comes back.
//!
//! Each of those issues pinned the property by example, after the fact.
//! This module states it once and runs it over a table, so a new spelling
//! is a table row rather than a new test — and so the *negative* half
//! (distinct links must not merge) is exercised with the same weight as
//! the positive half. Over-normalizing is the worse failure: a missed
//! anchor loses one link's lid, whereas a merged anchor hands each link
//! the other's live lid through `resolve_lid_batch`'s FIFO.
//!
//! Scope: this covers the *comparison* layer end to end, driving the
//! `templatize` → `prepare_field` seam rather than hand-writing the
//! templatized body. Most per-file unit tests do hand-write it, so a case
//! that fails to templatize at all reads green there — but not all of
//! them: `braze_managed`'s `plaintext_lid_round_trips_whatever_the_filter_spacing`
//! and `plaintext_lid_survives_a_content_blocks_include_in_the_url` drive
//! the same seam, and assert full-body equality against
//! `t.new_body.replace(TOKEN, …)`, which is *stricter* than the multiset
//! recovery asserted here. This table is breadth over those two, not a
//! replacement for them.
//!
//! The table also turned up two boundaries, pinned here rather than fixed
//! because each changes correlation semantics and wants its own
//! regression cases — a test that states a boundary is what stops it
//! being rediscovered as a bug:
//!
//! - #84, an asymmetry in which elements each side accepts as an anchor.
//!   Fails safe (fatal `UnresolvedLid`).
//! - #85 (fixed): an include whose `${NAME}` contains whitespace is still
//!   left unmanaged by `templatize` — Braze forbids whitespace in a real
//!   content block name, so such an include can never be templatized —
//!   but `correlation::cb_id_filter_re` now masks the raw `cbN` out of
//!   the anchor key regardless, so a nearby `lid` no longer inherits its
//!   instability. See `whitespace_named_include_no_longer_corrupts_a_live_lid`.

use crate::values::braze_managed::prepare_field;
use crate::values::correlation::{extract_cb_id_values, extract_lid_values_unanchored};
use crate::values::placeholder::TOKEN;
use crate::values::templatize::{templatize_body, FieldKind};

/// Live lid values in appearance order.
fn lids(body: &str) -> Vec<String> {
    extract_lid_values_unanchored(body)
}

/// Live `cbN` values in appearance order.
fn cb_ids(body: &str) -> Vec<String> {
    extract_cb_id_values(body)
        .into_iter()
        .map(|c| c.value)
        .collect()
}

/// Count `| lid:` filter occurrences, whatever value spelling follows.
///
/// Deliberately blind to the value: that is what lets it catch a lid
/// `templatize` failed to recognize, which is the one thing a count taken
/// from the correlation regexes cannot do. Anchoring on the preceding
/// pipe keeps prose (`<p>lid: see below</p>`) out of the count, and
/// tolerating whitespace keeps a raw `{{x|lid:'…'}}` in it.
fn lid_filters(body: &str) -> usize {
    body.match_indices("lid:")
        .filter(|(i, _)| body[..*i].trim_end().ends_with('|'))
        .count()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// The invariant, for a body that reaches Braze and comes back respelled.
///
/// `authored` is the body as it stood when `templatize` ran; `remote` is
/// what Braze hands back. They denote the same links, possibly spelled
/// differently — the dashboard reformats on save, and `templatize` itself
/// rewrites the filter, so the two are rarely byte-identical even when
/// nobody edited anything.
///
/// Recovery is asserted as a multiset, not a sequence: a remote may list
/// the links in a different order than the template, and the property
/// under test here is that no value is *lost*. Whether each value lands on
/// the right link is the separate concern of [`assert_keeps_pairing`].
fn assert_survives_reformat(authored: &str, remote: &str, field: FieldKind) {
    let t = templatize_body(authored, field);

    // Guard before the real assertions: a case that templatizes nothing
    // would satisfy everything below vacuously.
    assert!(
        t.new_body.contains(TOKEN),
        "case templatized nothing, so it cannot test correlation: {authored}"
    );

    // `contains(TOKEN)` only rules out templatizing *nothing*; a row that
    // templatizes one of two links would still ride through. Closing that
    // needs a check the correlation regexes cannot supply: `lids()` and
    // `templatize`'s own detector are the same pattern, so comparing their
    // counts holds for *any* input — including one whose lid spelling
    // neither side recognizes, which is exactly the row that would slip.
    // `lid` is Braze-reserved, so counting the filter by name alone is
    // independent of both patterns and does catch it.
    assert_eq!(
        lid_filters(&t.new_body),
        t.new_body.matches("lid: '__BRAZESYNC__'").count(),
        "templatize left a raw lid filter behind — its detector does not \
         recognize that spelling, so the row tests nothing for that link: {authored}"
    );
    // The same argument does not extend to `id:`: it is not reserved, so a
    // template may carry its own. Here the counts are a drift check between
    // `templatize::cb_id_match_re` and `correlation::cb_id_include_re`,
    // which are duplicated patterns that must stay in step.
    assert_eq!(
        t.cb_id_rewrites,
        cb_ids(authored).len(),
        "the two cb_id patterns have drifted apart in: {authored}"
    );

    let p = prepare_field(&t.new_body, Some(remote), field);
    assert!(p.errors.is_empty(), "{:?} for: {authored}", p.errors);
    // A fallback here is the family's signature failure: the anchor missed,
    // so a generated slug is about to be POSTed over a live identifier.
    assert!(
        p.fallbacks.is_empty(),
        "live value replaced by a fallback slug: {:?}\n  authored: {authored}\n  remote:   {remote}",
        p.fallbacks
    );
    assert!(p.warnings.is_empty(), "{:?} for: {authored}", p.warnings);
    assert_eq!(
        sorted(lids(&p.body)),
        sorted(lids(remote)),
        "lid values not recovered\n  authored: {authored}\n  remote:   {remote}\n  got:      {}",
        p.body
    );
    assert_eq!(
        sorted(cb_ids(&p.body)),
        sorted(cb_ids(remote)),
        "cb_id values not recovered\n  authored: {authored}\n  remote:   {remote}\n  got:      {}",
        p.body
    );
}

/// The other half: each live value must land on the link it belongs to.
///
/// [`assert_survives_reformat`] catches a *merge* only incidentally: two
/// anchors collapsed into one bucket lose no value and fire no fallback,
/// but `resolve_lid_batch` does warn on a bucket holding more than one
/// remote occurrence, and that assertion is already made above. What it
/// cannot see is a transposition that preserves bucket sizes — template
/// keys `k1, k2` against remote keys `k2, k1`, where every bucket holds
/// exactly one value and no warning fires. `expected` names fragments that
/// must appear verbatim in the resolved body, which pins value-to-link
/// placement independently of either signal.
///
/// These cases put the two links in *opposite* order on the remote side,
/// so a merged bucket hands out the wrong value rather than accidentally
/// the right one.
fn assert_keeps_pairing(authored: &str, remote: &str, field: FieldKind, expected: &[&str]) {
    assert_survives_reformat(authored, remote, field);
    let t = templatize_body(authored, field);
    let p = prepare_field(&t.new_body, Some(remote), field);
    for fragment in expected {
        assert!(
            p.body.contains(fragment),
            "expected {fragment:?} in resolved body\n  got: {}",
            p.body
        );
    }
}

#[test]
fn identity_round_trip_across_shapes() {
    // The floor: nothing was reformatted, so any failure here is a bug in
    // the templatize/resolve seam itself rather than in normalization.
    let cases: &[(&str, FieldKind)] = &[
        (
            r#"<a href="https://x.com/sale">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
        ),
        (
            r#"<a href="https://x.com/a">{{x | lid: 'liveaaaaaaaa1'}}</a>
               <a href="https://x.com/b">{{y | lid: 'liveaaaaaaaa2'}}</a>"#,
            FieldKind::EmailHtmlBody,
        ),
        // #68: the query separator is Liquid, so the key holds no literal `?`.
        (
            r#"<a href="https://x.com/p{{sep}}lid={{x | lid: 'liveaaaaaaaa1'}}">go</a>"#,
            FieldKind::ContentBlock,
        ),
        // #73: an include inside the URL puts braces inside the tag.
        (
            r#"<a href="https://x.com/{{content_blocks.${cta} | id: 'cb1'}}/p">go</a>{{x | lid: 'liveaaaaaaaa1'}}"#,
            FieldKind::ContentBlock,
        ),
        // Non-anchor element: VML / SVG CTAs route through the same path,
        // for the shape where the lid sits inside the href — see
        // `lid_in_a_non_anchor_element_body_has_no_template_side_anchor`.
        (
            r#"<v:roundrect href="https://x.com/p?lid={{x | lid: 'liveaaaaaaaa1'}}">go</v:roundrect>"#,
            FieldKind::EmailHtmlBody,
        ),
        // #70: plaintext, where the run's extent is the fragile part.
        (
            "Go https://x.com/promo {{x | lid: 'liveaaaaaaaa1'}} now",
            FieldKind::EmailPlainBody,
        ),
        (
            "Go https://x.com/p{{sep}}lid={{x | lid: 'liveaaaaaaaa1'}} now",
            FieldKind::EmailPlainBody,
        ),
        (
            "Go https://x.com/{{content_blocks.${cta} | id: 'cb1'}}/p{{sep}}lid={{x | lid: 'liveaaaaaaaa1'}} now",
            FieldKind::EmailPlainBody,
        ),
        // Two plaintext links with only a tag between them and no
        // whitespace at all, so the single `plaintext_url_re` run has to be
        // cut by `split_on_embedded_scheme`. Written with a space, the run
        // would end at the space instead and this row would pin nothing.
        (
            "https://x.com/one{{x | lid: 'liveaaaaaaaa1'}}https://x.com/two{{y | lid: 'liveaaaaaaaa2'}}",
            FieldKind::EmailPlainBody,
        ),
        // No anchor exists; resolution is positional.
        (
            "Sale {{x | lid: 'liveaaaaaaaa1'}}",
            FieldKind::EmailSubject,
        ),
    ];
    for (body, field) in cases {
        assert_survives_reformat(body, body, *field);
    }
}

#[test]
fn survives_every_respelling_the_dashboard_can_introduce() {
    // The positive half of the family. Each row is the same link spelled
    // two ways; the live lid must come through rather than be replaced by
    // a generated slug. Rows are grouped by the spelling axis so a new
    // axis is a new row, not a new test.
    let cases: &[(&str, &str, FieldKind)] = &[
        // #68 / #70: the filter itself respaced. `templatize` canonicalizes
        // to `| lid: '…'`, so this axis moves on almost every real body.
        (
            r#"<a href="https://x.com/p{{sep}}lid={{x|lid:'liveaaaaaaaa1'}}">go</a>"#,
            r#"<a href="https://x.com/p{{sep}}lid={{ x | lid: 'liveaaaaaaaa1' }}">go</a>"#,
            FieldKind::ContentBlock,
        ),
        // #77: whitespace the filter mask does not reach — after `{{`,
        // before `}}`, and around an unrelated filter.
        (
            r#"<a href="https://x.com/p{{sep}}lid={{x|lid:'liveaaaaaaaa1'}}">go</a>"#,
            r#"<a href="https://x.com/p{{ sep }}lid={{ x | lid: 'liveaaaaaaaa1' }}">go</a>"#,
            FieldKind::ContentBlock,
        ),
        (
            "Go https://x.com/p{{sep|default:'?'}}lid={{x|lid:'liveaaaaaaaa1'}} now",
            "Go https://x.com/p{{ sep | default: '?' }}lid={{ x | lid: 'liveaaaaaaaa1' }} now",
            FieldKind::EmailPlainBody,
        ),
        // Liquid whitespace control added or removed around a variable.
        (
            r#"<a href="https://x.com/{{-sep-}}p">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            r#"<a href="https://x.com/{{- sep -}}p">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
        ),
        // Padding around a `${NAME}` is formatting; the name is the same.
        (
            r#"<a href="https://x.com/{{custom_attribute.${plan}}}/p">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            r#"<a href="https://x.com/{{ custom_attribute.${ plan } }}/p">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
        ),
        // Quote style on the managed value itself. Both patterns accept
        // either, so this must not depend on which the dashboard emits.
        (
            r#"<a href="https://x.com/sale">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            r#"<a href="https://x.com/sale">{{x | lid: "liveaaaaaaaa1"}}</a>"#,
            FieldKind::ContentBlock,
        ),
        // The include respaced. `templatize` rebuilds it canonically, so
        // the key has to be derived from the name rather than the bytes.
        (
            r#"<a href="https://x.com/{{content_blocks.${cta} | id: 'cb1'}}/p">go</a>{{x | lid: 'liveaaaaaaaa1'}}"#,
            r#"<a href="https://x.com/{{content_blocks.${cta}|id:'cb1'}}/p">go</a>{{x | lid: 'liveaaaaaaaa1'}}"#,
            FieldKind::ContentBlock,
        ),
        // Attribute quote style, and an attribute reordered around it.
        (
            r#"<a href="https://x.com/sale" class="cta">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            r#"<a class="cta" href='https://x.com/sale'>{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
        ),
        // A query string appears (or changes) on the remote side. The key
        // cuts at the first `?` outside a tag, so tracking params added in
        // the dashboard must not cost the lid.
        (
            r#"<a href="https://x.com/sale">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            r#"<a href="https://x.com/sale?utm_source=braze">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
        ),
    ];
    for (authored, remote, field) in cases {
        assert_survives_reformat(authored, remote, *field);
    }
}

#[test]
fn respelling_never_merges_two_distinct_links() {
    // The negative half, and the one that matters more: a merge is silent
    // and hands each link the other's live identifier. Every row differs
    // in exactly one byte-level detail that the normalizer must treat as
    // identity rather than formatting, and every remote lists the links in
    // reverse order so a merged FIFO bucket transposes rather than
    // accidentally agreeing.
    let cases: &[(&str, &str, FieldKind, &[&str])] = &[
        // Spaces inside a quoted argument are the value, not layout.
        (
            "https://x.com/{{sep|default:' - '}}{{x|lid:'liveaaaaaaaa1'}} \
             https://x.com/{{sep|default:'-'}}{{x|lid:'liveaaaaaaaa2'}}",
            "https://x.com/{{ sep | default: '-' }}{{ x | lid: 'liveaaaaaaaa2' }} \
             https://x.com/{{ sep | default: ' - ' }}{{ x | lid: 'liveaaaaaaaa1' }}",
            FieldKind::EmailPlainBody,
            &[
                "{{sep|default:' - '}}{{x| lid: 'liveaaaaaaaa1'}}",
                "{{sep|default:'-'}}{{x| lid: 'liveaaaaaaaa2'}}",
            ],
        ),
        // Whitespace *between* the bytes of a `${NAME}` is part of the name.
        (
            "https://x.com/{{custom_attribute.${first name}}}{{x|lid:'liveaaaaaaaa1'}} \
             https://x.com/{{custom_attribute.${firstname}}}{{x|lid:'liveaaaaaaaa2'}}",
            "https://x.com/{{ custom_attribute.${firstname} }}{{ x | lid: 'liveaaaaaaaa2' }} \
             https://x.com/{{ custom_attribute.${first name} }}{{ x | lid: 'liveaaaaaaaa1' }}",
            FieldKind::EmailPlainBody,
            &[
                "{{custom_attribute.${first name}}}{{x| lid: 'liveaaaaaaaa1'}}",
                "{{custom_attribute.${firstname}}}{{x| lid: 'liveaaaaaaaa2'}}",
            ],
        ),
        // Plain text after the managed tag distinguishes two links; the run
        // spans the tag precisely so this stays visible.
        (
            "https://x.com/p{{x|lid:'liveaaaaaaaa1'}}/alpha \
             https://x.com/p{{x|lid:'liveaaaaaaaa2'}}/beta",
            "https://x.com/p{{ x | lid: 'liveaaaaaaaa2' }}/beta \
             https://x.com/p{{ x | lid: 'liveaaaaaaaa1' }}/alpha",
            FieldKind::EmailPlainBody,
            &[
                "{{x| lid: 'liveaaaaaaaa1'}}/alpha",
                "{{x| lid: 'liveaaaaaaaa2'}}/beta",
            ],
        ),
        // Two different include names in two different links.
        (
            r#"<a href="https://x.com/{{content_blocks.${a} | id: 'cb1'}}">{{x | lid: 'liveaaaaaaaa1'}}</a>
               <a href="https://x.com/{{content_blocks.${b} | id: 'cb2'}}">{{y | lid: 'liveaaaaaaaa2'}}</a>"#,
            r#"<a href="https://x.com/{{content_blocks.${b}|id:'cb2'}}">{{y | lid: 'liveaaaaaaaa2'}}</a>
               <a href="https://x.com/{{content_blocks.${a}|id:'cb1'}}">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
            &[
                r#"{{content_blocks.${a} | id: 'cb1'}}">{{x | lid: 'liveaaaaaaaa1'}}"#,
                r#"{{content_blocks.${b} | id: 'cb2'}}">{{y | lid: 'liveaaaaaaaa2'}}"#,
            ],
        ),
        // #85: an include's `${NAME}` may itself contain whitespace. It
        // stays unmanaged by `templatize` (Braze forbids whitespace in a
        // real content block name), but the masking that stabilizes a
        // nearby lid's anchor key must still tell "a b" and "ab" apart —
        // trim removes padding only, never interior bytes — or the two
        // lids collapse onto one FIFO bucket. The cb_id value is held
        // fixed (not reassigned) so the unmanaged include's raw `cbN`
        // trivially matches remote; only the lid pairing is under test.
        (
            r#"<a href="https://x.com/{{content_blocks.${a b} | id: 'cb1'}}">{{x | lid: 'liveaaaaaaaa1'}}</a>
               <a href="https://x.com/{{content_blocks.${ab} | id: 'cb1'}}">{{y | lid: 'liveaaaaaaaa2'}}</a>"#,
            r#"<a href="https://x.com/{{content_blocks.${ab}|id:'cb1'}}">{{y | lid: 'liveaaaaaaaa2'}}</a>
               <a href="https://x.com/{{content_blocks.${a b}|id:'cb1'}}">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::ContentBlock,
            &[
                r#"{{content_blocks.${a b} | id: 'cb1'}}">{{x | lid: 'liveaaaaaaaa1'}}"#,
                r#"{{content_blocks.${ab} | id: 'cb1'}}">{{y | lid: 'liveaaaaaaaa2'}}"#,
            ],
        ),
        // Different path tails, same everything else.
        (
            r#"<a href="https://x.com/alpha">{{x | lid: 'liveaaaaaaaa1'}}</a>
               <a href="https://x.com/beta">{{x | lid: 'liveaaaaaaaa2'}}</a>"#,
            r#"<a href="https://x.com/beta">{{x | lid: 'liveaaaaaaaa2'}}</a>
               <a href="https://x.com/alpha">{{x | lid: 'liveaaaaaaaa1'}}</a>"#,
            FieldKind::EmailHtmlBody,
            &[
                r#"https://x.com/alpha">{{x | lid: 'liveaaaaaaaa1'}}"#,
                r#"https://x.com/beta">{{x | lid: 'liveaaaaaaaa2'}}"#,
            ],
        ),
    ];
    for (authored, remote, field, expected) in cases {
        assert_keeps_pairing(authored, remote, *field, expected);
    }
}

#[test]
fn whitespace_named_include_no_longer_corrupts_a_live_lid() {
    // #85, fixed. `templatize` still declines to manage an include whose
    // `${NAME}` holds whitespace — Braze forbids whitespace in a real
    // content block name, so such an include can never denote one — but
    // it now says so via a warning instead of staying silent, and
    // `correlation::cb_id_filter_re` masks the raw `cbN` out of the
    // anchor key regardless of whether the name is manageable. A nearby
    // `lid` no longer inherits the instability of an include that can
    // never correlate.
    let authored = r#"<a href="https://x.com/{{content_blocks.${Plan (US)} | id: 'cb1'}}/p">{{x | lid: 'liveaaaaaaaa1'}}</a>"#;
    let t = templatize_body(authored, FieldKind::ContentBlock);
    assert_eq!(t.lid_rewrites, 1);
    assert_eq!(
        t.cb_id_rewrites, 0,
        "still not managed — Braze cannot name a content block with whitespace"
    );
    assert!(t.new_body.contains("| id: 'cb1'"), "got: {}", t.new_body);
    assert!(
        t.warnings.iter().any(|w| w.contains("whitespace")),
        "expected a warning about the invalid content block name, got: {:?}",
        t.warnings
    );

    let p = prepare_field(&t.new_body, Some(authored), FieldKind::ContentBlock);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert!(p.fallbacks.is_empty(), "{:?}", p.fallbacks);
    assert_eq!(lids(&p.body), vec!["liveaaaaaaaa1".to_string()]);

    // Braze reassigns the (never-managed) cb_id. This used to overwrite
    // the live lid with a path-tail slug, because the raw `cbN` sat
    // inside the anchor key and the reassignment made the template-side
    // and remote-side keys diverge. It no longer does: `cb_id_filter_re`
    // masks the `cbN` out of the key regardless, so the key stays stable
    // and the lid anchor still matches.
    let remote_reassigned = authored.replace("| id: 'cb1'", "| id: 'cb7'");
    let p = prepare_field(
        &t.new_body,
        Some(&remote_reassigned),
        FieldKind::ContentBlock,
    );
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert!(
        p.fallbacks.is_empty(),
        "the live lid must survive cb_id reassignment: {:?}",
        p.fallbacks
    );
    assert_eq!(
        lids(&p.body),
        vec!["liveaaaaaaaa1".to_string()],
        "the live lid must not be overwritten by a slug: {}",
        p.body
    );
}

#[test]
fn blank_named_include_no_longer_corrupts_a_live_lid() {
    // Same fix, the other invalid-name case: an all-padding/empty
    // `${NAME}` (`cb_id_filter_re`'s capture is `*`, not `+`, precisely
    // so this masks too). Unit-tested in isolation elsewhere
    // (`correlation::cb_id_filter_masks_a_name_containing_whitespace`,
    // `templatize::cb_id_include_with_blank_name_is_left_untemplated_with_warning`)
    // but not previously driven through the full templatize → resolve →
    // reassign round trip the way the whitespace case is above.
    let authored = r#"<a href="https://x.com/{{content_blocks.${} | id: 'cb1'}}/p">{{x | lid: 'liveaaaaaaaa1'}}</a>"#;
    let t = templatize_body(authored, FieldKind::ContentBlock);
    assert_eq!(t.lid_rewrites, 1);
    assert_eq!(t.cb_id_rewrites, 0, "a blank name is never managed");
    assert!(
        t.warnings.iter().any(|w| w.contains("is empty")),
        "expected a warning about the blank content block name, got: {:?}",
        t.warnings
    );

    let remote_reassigned = authored.replace("| id: 'cb1'", "| id: 'cb7'");
    let p = prepare_field(
        &t.new_body,
        Some(&remote_reassigned),
        FieldKind::ContentBlock,
    );
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert!(
        p.fallbacks.is_empty(),
        "the live lid must survive cb_id reassignment: {:?}",
        p.fallbacks
    );
    assert_eq!(
        lids(&p.body),
        vec!["liveaaaaaaaa1".to_string()],
        "the live lid must not be overwritten by a slug: {}",
        p.body
    );
}

#[test]
fn lid_in_a_non_anchor_element_body_has_no_template_side_anchor() {
    // Second known boundary (#84), found by the table above. The two sides
    // do not agree on which elements can carry an anchor:
    //
    // - remote: `correlation::href_re` takes `href` / `src` / `action` on
    //   *any* element, so it extracts a pair here;
    // - template: once the lid sits past the `>`, `lid_anchor_for` falls
    //   through to `braze_managed::anchor_href_re`, which is `<a>`-only.
    //
    // A VML CTA whose lid is inside the href is supported (that shape stays
    // within the open tag and goes through `url_attr_re`); one whose lid is
    // in the element *body* is not.
    //
    // Pinned rather than fixed because it fails in the safe direction: the
    // result is a fatal `UnresolvedLid`, so the operator is stopped rather
    // than having a live identifier quietly overwritten by a slug. Widening
    // `anchor_href_re` to match `href_re` would resolve it, but that changes
    // which element encloses an anchor and so is a correlation-semantics
    // change, not a test fix.
    let body = r#"<v:rect href="https://x.com/sale">{{x | lid: 'liveaaaaaaaa1'}}</v:rect>"#;
    let t = templatize_body(body, FieldKind::EmailHtmlBody);
    assert_eq!(t.lid_rewrites, 1);

    // The remote side does find the pair.
    assert_eq!(
        crate::values::correlation::extract_html_lid_values(body)
            .into_iter()
            .map(|c| c.value)
            .collect::<Vec<_>>(),
        vec!["liveaaaaaaaa1".to_string()],
    );

    // The template side does not, and says so loudly.
    let p = prepare_field(&t.new_body, Some(body), FieldKind::EmailHtmlBody);
    assert!(
        p.errors.iter().any(|e| matches!(
            e,
            crate::values::placeholder::ResolutionError::UnresolvedLid { .. }
        )),
        "expected a fatal UnresolvedLid, got: {:?}",
        p.errors
    );
    assert!(
        p.fallbacks.is_empty(),
        "must not POST a slug: {:?}",
        p.fallbacks
    );
}

#[test]
fn the_vacuity_guard_is_not_itself_vacuous() {
    // The guard this replaced compared two counts taken from the same
    // regex, so it held for every input. A guard nobody tests is how that
    // goes unnoticed, so this pins both directions.

    // Prose that merely contains `lid:` is not a filter and must not fire.
    let prose =
        r#"<p>lid: see below</p><a href="https://x.com/s">{{x | lid: '__BRAZESYNC__'}}</a>"#;
    assert_eq!(lid_filters(prose), 1);

    // A raw filter whose value spelling `templatize` does not recognize is
    // left behind, and the guard must see it — `lids()` cannot, because it
    // does not recognize that spelling either.
    let body = r#"<a href="https://x.com/a">{{x | lid: 'liveaaaaaaaa1'}}</a>
                  <a href="https://x.com/b">{{y|lid:'Live-2'}}</a>"#;
    let t = templatize_body(body, FieldKind::EmailHtmlBody);
    assert_eq!(lids(body).len(), 1, "the correlation regex misses it too");
    assert_eq!(lid_filters(&t.new_body), 2, "got: {}", t.new_body);
    assert_eq!(t.new_body.matches("lid: '__BRAZESYNC__'").count(), 1);
}
